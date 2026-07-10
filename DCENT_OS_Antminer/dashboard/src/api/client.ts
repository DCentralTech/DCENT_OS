// DCENTos REST API Client — typed fetch wrappers for all endpoints

import type {
  StatusResponse, ConfigResponse, ConfigUpdateRequest, ConfigUpdateResponse,
  DonationConfig, DonationConfigResponse, DonationInfoResponse,
  DashboardVersionResponse,
  SystemInfoResponse, SystemHealthResponse, SystemStatsResponse, SystemUpgradeStatusResponse, SystemAsicResponse,
  PsuOverrideRequest, PsuOverrideResponse,
  ApiCompatibilityManifestResponse,
  CompetitiveReadinessResponse,
  NetworkBlockResponse,
  MiningWorkPostureResponse,
  MiningPipelineManifestResponse,
  MiningPipelineSnapshot,
  MiningPipelineSnapshotSchemaResponse,
  ThermalPowerPostureResponse,
  DeviceCapabilityDescriptor,
  ConfigBackupManifestResponse, ConfigExportResponse, ConfigImportResponse,
  PoolsResponse, PoolConfigRequest, PoolConfigResponse, PoolTestResponse,
  StatsResponse, RollingMetricsResponse, HistoryPoint, HistoryResponse, ProfilesResponse, RecentShareEvent, ShareHistoryResponse,
  AutotunerStatusResponse, AutotunerChipHealthResponse, AutotunerVisibilityResponse,
  ChipHealthSnapshotResponse,
  HeaterStatusResponse, HeaterTargetRequest, HeaterTargetResponse,
  HeaterPresetsResponse, RoomTempRequest, NightModeResponse, NightModeRequest,
  ActionResponse, FirmwareUploadResponse,
  PowerCalibrationRequest, PowerCalibrationResponse,
  PsuControlRequest, PsuControlResponse,
  RegisterReadResponse, RegisterWriteRequest,
  I2cReadResponse, I2cWriteRequest,
  AsicCommandRequest, AsicCommandResponse,
  PidStateResponse, PidParamsRequest,
  ChipFrequencyRequest, ChipVoltageRequest, ChipVoltageResponse,
  DiagnosticStartRequest, DiagnosticStartResponse,
  DiagnosticStatusResponse, DiagnosticResultResponse, RecentDiagnosticReportsResponse, LogManifestResponse,
  DiagnosticsFailureModesResponse, ChainDiagnosticsResponse, LocalRejectsResponse,
  HardwarePicInfoResponse, RecoveryActionsResponse, SystemBootTimelineResponse,
  HistoryAuditResponse, PsuCatalogResponse, CgminerCatalogResponse, ReCatalogIndexResponse,
  NetworkTroubleshootResponse, PsuTroubleshootResponse, FpgaTroubleshootResponse,
  LedStatusResponse, LedPatternsResponse, LocateRequest, LedConfigResponse, LedConfigUpdateRequest,
  WebhookConfig, WebhookConfigUpdateRequest, WebhookConfigUpdateResponse, WebhookTestResponse,
  Sv2StatusResponse, Sv2HandshakeResponse, Sv2MessagesResponse,
  JobDeclarationStatus, JobDeclarationConfig,
  OperatingMode, SetupStatusResponse, SetupCircuitRequest, SetupEconomicsRequest,
  OffGridConfigPayload, OffGridConfigResponse, OffGridConfigSaveResponse, OffGridPresetsResponse, OffGridProbeResponse, OffGridStatusResponse,
  NetworkHostnameResponse, NetworkInfoResponse, MinerTypeResponse,
  PvtTableResponse,
  BootPhaseResponse, BootTimelineResponse,
  SiliconReportResponse, ThermalSupervisorSnapshot, AuditLogResponse,
} from './types';
import type {
  FleetDiscoverRequest,
  FleetDiscoverResponse,
  InverterBrand,
  MqttConfig,
  MqttStatusResponse,
  MqttTestResponse,
  SolarConfig,
  SolarStatus,
  SolarTestResponse,
  SolarVerificationHistoryResponse,
  SolarVerificationSample,
} from './feature-types';
import {
  getSessionToken,
  setSessionToken,
  getVolatilePassword,
  setVolatilePassword,
} from './credentials';
import { dropBootZeroHistory } from '../utils/history';

// API base: always same-origin. In production, server.py proxies /api/* to dcentrald:8080.
// In Vite dev mode, vite.config.ts proxy handles the forwarding.
const BASE = '';

export const DAEMON_DISCONNECTED_EVENT = 'dcentos:daemon-disconnected';
export const DAEMON_RECONNECTED_EVENT = 'dcentos:daemon-reconnected';

type ApiErrorOptions = {
  code?: string;
  detail?: string;
  suggestion?: string;
};

class ApiError extends Error {
  public code?: string;
  public detail?: string;
  public suggestion?: string;

  constructor(public status: number, message: string, options: ApiErrorOptions = {}) {
    super(message);
    this.name = 'ApiError';
    this.code = options.code;
    this.detail = options.detail;
    this.suggestion = options.suggestion;
  }
}

type StoredAuthSettings = {
  apiToken?: string | null;
  password?: string | null;
};

type SessionResponse = {
  session_token?: string | null;
  api_token?: string | null;
  session?: {
    session_token?: string | null;
  };
};

// Auth credentials are owned by ./credentials — the revocable session token in
// sessionStorage and the password in memory only. They are deliberately NOT
// read from / written to the durable `dcentos-settings` blob, so this writer
// can no longer race-clobber the settings store (and vice-versa).
function loadAuthSettings(): StoredAuthSettings {
  return { apiToken: getSessionToken(), password: getVolatilePassword() };
}

function saveAuthSettings(update: Partial<StoredAuthSettings>) {
  if ('apiToken' in update) setSessionToken(update.apiToken ?? null);
  if ('password' in update) setVolatilePassword(update.password ?? null);
}

function extractSessionToken(data: SessionResponse | null | undefined): string | null {
  return data?.session_token || data?.api_token || data?.session?.session_token || null;
}

function dispatchDaemonEvent(name: string) {
  if (typeof window === 'undefined') {
    return;
  }
  window.dispatchEvent(new CustomEvent(name));
}

function stringField(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() ? value.trim() : undefined;
}

function parseApiErrorText(status: number, body: string): ApiError {
  const trimmed = body.trim();
  if (!trimmed) return new ApiError(status, `Request failed with status ${status}`);
  try {
    const parsed = JSON.parse(trimmed) as unknown;
    if (typeof parsed === 'string' && parsed.trim()) {
      return new ApiError(status, parsed.trim());
    }
    if (parsed && typeof parsed === 'object') {
      const record = parsed as {
        error?: unknown;
        message?: unknown;
        detail?: unknown;
        code?: unknown;
        suggestion?: unknown;
      };
      const message =
        stringField(record.error) ??
        stringField(record.message) ??
        stringField(record.detail) ??
        `Request failed with status ${status}`;
      return new ApiError(status, message, {
        code: stringField(record.code),
        detail: stringField(record.detail),
        suggestion: stringField(record.suggestion),
      });
    }
  } catch {
    // Plain text bodies are valid on older daemons.
  }
  return new ApiError(status, trimmed);
}

export async function parseApiError(res: Response): Promise<ApiError> {
  return parseApiErrorText(res.status, await res.text());
}

async function createSessionFromPassword(
  password: string,
  opts: { throwOnError?: boolean } = {},
): Promise<string | null> {
  const res = await fetch(`${BASE}/api/auth/session`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ password, label: 'dashboard' }),
  });

  if (!res.ok) {
    if (res.status === 401 || res.status === 403) {
      saveAuthSettings({ apiToken: null });
    }
    if (opts.throwOnError) {
      throw await parseApiError(res);
    }
    return null;
  }

  const data = await res.json() as SessionResponse;
  const token = extractSessionToken(data);
  if (token) {
    saveAuthSettings({ apiToken: token });
  }
  return token;
}

/** Read or bootstrap the Bearer session token from persisted settings. */
async function getAuthHeaders(): Promise<Record<string, string>> {
  const settings = loadAuthSettings();
  if (settings.apiToken) {
    return { 'Authorization': `Bearer ${settings.apiToken}` };
  }
  if (settings.password) {
    const token = await createSessionFromPassword(settings.password);
    if (token) {
      return { 'Authorization': `Bearer ${token}` };
    }
  }
  return {};
}

/**
 * Default per-request timeout (ms). A wedged daemon that accepts the TCP
 * connection but never responds (the .25/.139-class bring-up states this
 * firmware lives in) otherwise leaves `fetch` pending on the browser default
 * (often minutes), so panels that schedule their next poll/retry inside the
 * in-flight promise never re-arm — the UI sits on a spinner forever with no
 * error and no recovery (DASH-STATE-1). A hard timeout converts every silent
 * hang into a clean error the existing panel catch-blocks already handle.
 * Firmware upload uses a separate XHR path with its own progress handling and
 * is unaffected.
 */
const DEFAULT_REQUEST_TIMEOUT_MS = 15000;

/** Thrown when a request exceeds its timeout — distinct from a caller cancel. */
export class ApiTimeoutError extends Error {
  constructor(path: string, ms: number) {
    super(`Request to ${path} timed out after ${ms}ms`);
    this.name = 'ApiTimeoutError';
  }
}

/**
 * `fetch` with a hard timeout via AbortController, composed with any
 * caller-supplied `signal` so an explicit cancel still propagates. On timeout
 * it aborts the request and throws {@link ApiTimeoutError}; a caller-initiated
 * abort rethrows the original error.
 */
async function fetchWithTimeout(
  url: string,
  init: RequestInit,
  timeoutMs: number,
  path: string,
): Promise<Response> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  const callerSignal = init.signal ?? undefined;
  const onAbort = () => controller.abort();
  if (callerSignal) {
    if (callerSignal.aborted) controller.abort();
    else callerSignal.addEventListener('abort', onAbort, { once: true });
  }
  try {
    return await fetch(url, { ...init, signal: controller.signal });
  } catch (err) {
    if (controller.signal.aborted && !callerSignal?.aborted) {
      throw new ApiTimeoutError(path, timeoutMs);
    }
    throw err;
  } finally {
    clearTimeout(timer);
    callerSignal?.removeEventListener('abort', onAbort);
  }
}

async function request(
  path: string,
  init: RequestInit = {},
  timeoutMs: number = DEFAULT_REQUEST_TIMEOUT_MS,
): Promise<Response> {
  const authHeaders = await getAuthHeaders();
  const headers = new Headers(init.headers ?? {});
  Object.entries(authHeaders).forEach(([key, value]) => headers.set(key, value));

  let response: Response;
  try {
    response = await fetchWithTimeout(`${BASE}${path}`, { ...init, headers }, timeoutMs, path);
    if (response.ok) {
      dispatchDaemonEvent(DAEMON_RECONNECTED_EVENT);
    }
  } catch (err) {
    dispatchDaemonEvent(DAEMON_DISCONNECTED_EVENT);
    throw err;
  }

  if (response.status === 401) {
    const settings = loadAuthSettings();
    if (settings.password) {
      saveAuthSettings({ apiToken: null });
      const token = await createSessionFromPassword(settings.password);
      if (token) {
        const retryHeaders = new Headers(init.headers ?? {});
        retryHeaders.set('Authorization', `Bearer ${token}`);
        response = await fetchWithTimeout(`${BASE}${path}`, { ...init, headers: retryHeaders }, timeoutMs, path);
        if (response.ok) {
          dispatchDaemonEvent(DAEMON_RECONNECTED_EVENT);
        }
      }
    }
  }

  return response;
}

export const apiFetch = request;

type ApiOperatingMode = 'home' | 'standard' | 'hacker';
type RawStatusResponse = Omit<StatusResponse, 'mode'> & { mode: ApiOperatingMode };
type RawConfigResponse = Omit<ConfigResponse, 'mode'> & { mode: { active: ApiOperatingMode } };
type RawConfigUpdateRequest = Omit<ConfigUpdateRequest, 'mode'> & { mode?: { active: ApiOperatingMode } };
type RawSystemInfoResponse = Omit<SystemInfoResponse, 'mode'> & { mode: ApiOperatingMode };

function fromApiMode(mode: ApiOperatingMode): OperatingMode {
  return mode === 'home' ? 'heater' : mode;
}

function toApiMode(mode: OperatingMode): ApiOperatingMode {
  return mode === 'heater' ? 'home' : mode;
}

function normalizeStatus(status: RawStatusResponse): StatusResponse {
  return { ...status, mode: fromApiMode(status.mode) };
}

function normalizeConfig(config: RawConfigResponse): ConfigResponse {
  return {
    ...config,
    mode: {
      ...config.mode,
      active: fromApiMode(config.mode.active),
    },
  };
}

function normalizeSystemInfo(info: RawSystemInfoResponse): SystemInfoResponse {
  return { ...info, mode: fromApiMode(info.mode) };
}

function normalizeHistory(response: HistoryResponse): HistoryResponse {
  const stamped = (response.history ?? []).map((point: HistoryPoint) => ({
    ...point,
    timestamp: point.timestamp ?? point.timestamp_s ?? 0,
  }));
  return {
    ...response,
    // P3-35(a): trim the leading boot-zero placeholder rows (hashrate 0 + temp 0,
    // pool still "Connecting") so chart baselines/averages aren't dragged to the
    // floor by a fabricated 0/0 first point. Mid-stream zeros are preserved.
    history: dropBootZeroHistory(stamped),
  };
}

const DEFAULT_DONATION_CONFIG: DonationConfig = {
  enabled: true,
  percent: 2.0,
  pool_url: 'stratum+tcp://pool.d-central.tech:3333',
  worker: 'DungeonMaster',
  password: 'x',
  fallback_enabled: true,
  fallback_pool_url: 'stratum+tcp://stratum.braiins.com:3333',
  fallback_worker: 'DungeonMaster',
  fallback_password: 'x',
  cycle_duration_s: 3600,
};

type RawDonationPayload = Partial<DonationConfig> | DonationConfigResponse | null | undefined;

function clampDonationPercent(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value)
    ? Math.max(0, Math.min(5, value))
    : DEFAULT_DONATION_CONFIG.percent;
}

function clampDonationCycle(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value)
    ? Math.max(60, Math.min(86400, Math.round(value)))
    : DEFAULT_DONATION_CONFIG.cycle_duration_s;
}

function normalizeDonationConfig(payload: RawDonationPayload): DonationConfig {
  const value = payload && typeof payload === 'object' && 'config' in payload
    ? payload.config
    : payload;
  return {
    enabled: value?.enabled ?? DEFAULT_DONATION_CONFIG.enabled,
    percent: clampDonationPercent(value?.percent),
    pool_url: value?.pool_url || DEFAULT_DONATION_CONFIG.pool_url,
    worker: value?.worker || DEFAULT_DONATION_CONFIG.worker,
    password: value?.password || DEFAULT_DONATION_CONFIG.password,
    fallback_enabled: value?.fallback_enabled ?? DEFAULT_DONATION_CONFIG.fallback_enabled,
    fallback_pool_url: value?.fallback_pool_url || DEFAULT_DONATION_CONFIG.fallback_pool_url,
    fallback_worker: value?.fallback_worker || DEFAULT_DONATION_CONFIG.fallback_worker,
    fallback_password: value?.fallback_password || DEFAULT_DONATION_CONFIG.fallback_password,
    cycle_duration_s: clampDonationCycle(value?.cycle_duration_s),
  };
}

type RawRecentShareEvent = Partial<RecentShareEvent> & {
  timestampMs?: number;
  jobId?: string;
  targetDifficulty?: number | null;
  errorCode?: number | null;
  errorMsg?: string | null;
  workerName?: string | null;
  versionBits?: string | null;
  protocolMetaPresent?: boolean;
};

function normalizeShareEvent(event: RawRecentShareEvent): RecentShareEvent {
  return {
    timestamp_ms: event.timestamp_ms ?? event.timestampMs ?? 0,
    result: event.result ?? 'accepted',
    job_id: event.job_id ?? event.jobId ?? '',
    difficulty: event.difficulty ?? null,
    target_difficulty: event.target_difficulty ?? event.targetDifficulty ?? null,
    error_code: event.error_code ?? event.errorCode ?? null,
    error_msg: event.error_msg ?? event.errorMsg ?? null,
    worker_name: event.worker_name ?? event.workerName ?? null,
    nonce: event.nonce ?? null,
    ntime: event.ntime ?? null,
    extranonce2: event.extranonce2 ?? null,
    version_bits: event.version_bits ?? event.versionBits ?? null,
    version: event.version ?? null,
    protocol_meta_present: event.protocol_meta_present ?? event.protocolMetaPresent ?? false,
  };
}

function normalizeShareHistory(response: { events?: RawRecentShareEvent[] }): ShareHistoryResponse {
  return {
    events: (response.events ?? []).map(normalizeShareEvent),
  };
}

function toApiConfigUpdate(cfg: ConfigUpdateRequest): RawConfigUpdateRequest {
  if (!cfg.mode) {
    return cfg as RawConfigUpdateRequest;
  }

  return {
    ...cfg,
    mode: {
      ...cfg.mode,
      active: toApiMode(cfg.mode.active),
    },
  };
}

function isHttpUrl(value: string | undefined): boolean {
  return !!value && (value.startsWith('http://') || value.startsWith('https://'));
}

function normalizeTeslaHost(endpoint: string): string {
  const trimmed = endpoint.trim();
  if (!trimmed) return '';
  try {
    const url = new URL(trimmed.includes('://') ? trimmed : `http://${trimmed}`);
    return url.host;
  } catch {
    return trimmed.replace(/^https?:\/\//, '').replace(/\/.*$/, '');
  }
}

function buildTeslaEndpoint(config: SolarConfig): string {
  const host = config.teslaGatewayHost?.trim() || normalizeTeslaHost(config.apiEndpoint);
  if (!host) {
    return config.apiEndpoint;
  }
  return `http://${host}/api/meters/aggregates`;
}

function normalizeSolarProvider(provider: string | undefined, transport: string | undefined): InverterBrand | string | undefined {
  if (provider === 'victron' && transport === 'http-json') {
    return 'bridge';
  }
  return provider;
}

function normalizedMatchedFields<T extends { matched_fields?: string[]; matchedFields?: string[] }>(value: T): string[] | undefined {
  return value.matchedFields ?? value.matched_fields;
}

function normalizeAcceptedPayloadShapes<T extends { acceptedPayloadShapes?: string[] }>(value: T): string[] {
  return Array.isArray(value.acceptedPayloadShapes) ? value.acceptedPayloadShapes : [];
}

function normalizeSolarVerificationSample(sample: SolarVerificationSample): SolarVerificationSample {
  return {
    ...sample,
    provider: normalizeSolarProvider(sample.provider, sample.transport) || sample.provider,
    matched_fields: normalizedMatchedFields(sample),
  };
}

function normalizeSolarVerificationHistory(response: SolarVerificationHistoryResponse): SolarVerificationHistoryResponse {
  return {
    ...response,
    entries: response.entries.map(normalizeSolarVerificationSample),
  };
}

function normalizeSolarConfig(config: SolarConfig): SolarConfig {
  const normalizedBrand = normalizeSolarProvider(config.inverterBrand, isHttpUrl(config.apiEndpoint) ? 'http-json' : undefined);
  const brand = (normalizedBrand ?? config.inverterBrand) as InverterBrand;
  return {
    ...config,
    inverterBrand: brand,
    bridgeBaseUrl: brand === 'bridge' ? config.apiEndpoint : (config.bridgeBaseUrl ?? ''),
    bridgeApiKey: brand === 'bridge' ? config.apiKey : (config.bridgeApiKey ?? ''),
    teslaGatewayHost: brand === 'tesla' ? normalizeTeslaHost(config.apiEndpoint) : (config.teslaGatewayHost ?? ''),
    teslaPassword: brand === 'tesla' ? config.apiKey : (config.teslaPassword ?? ''),
    acceptedPayloadShapes: normalizeAcceptedPayloadShapes(config),
  };
}

function toApiSolarConfig(config: SolarConfig): SolarConfig {
  if (config.inverterBrand === 'bridge') {
    return {
      ...config,
      apiEndpoint: config.bridgeBaseUrl?.trim() || config.apiEndpoint,
      apiKey: config.bridgeApiKey?.trim() || config.apiKey,
    };
  }

  if (config.inverterBrand === 'tesla') {
    return {
      ...config,
      apiEndpoint: config.apiEndpoint.trim() || buildTeslaEndpoint(config),
      apiKey: config.teslaPassword?.trim() || config.apiKey,
    };
  }

  return config;
}

function normalizeSolarStatus(status: SolarStatus): SolarStatus {
  return {
    ...status,
    provider: normalizeSolarProvider(status.provider, status.transport),
    matched_fields: normalizedMatchedFields(status),
    acceptedPayloadShapes: normalizeAcceptedPayloadShapes(status),
  };
}

function normalizeSolarTestResponse(response: SolarTestResponse): SolarTestResponse {
  return {
    ...response,
    provider: normalizeSolarProvider(response.provider, response.transport) || response.provider,
    matched_fields: normalizedMatchedFields(response),
    acceptedPayloadShapes: normalizeAcceptedPayloadShapes(response),
  };
}

async function get<T>(path: string, timeoutMs?: number): Promise<T> {
  const res = await request(path, {}, timeoutMs);
  if (!res.ok) throw await parseApiError(res);
  return res.json();
}

async function post<T>(path: string, body?: unknown): Promise<T> {
  const res = await request(path, {
    method: 'POST',
    headers: body ? { 'Content-Type': 'application/json' } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) throw await parseApiError(res);
  return res.json();
}

// ─── Core ───────────────────────────────────────────────────
export const api = {
  createSession: async (password: string): Promise<string | null> =>
    createSessionFromPassword(password, { throwOnError: true }),
  revokeCurrentSession: async () => {
    const { apiToken } = loadAuthSettings();
    const res = await fetch(`${BASE}/api/auth/session/current`, {
      method: 'DELETE',
      headers: apiToken ? { 'Authorization': `Bearer ${apiToken}` } : {},
    });
    if (!res.ok) throw await parseApiError(res);
    saveAuthSettings({ apiToken: null });
    return res.json() as Promise<ActionResponse>;
  },
  // Status & Config
  getStatus: async () => normalizeStatus(await get<RawStatusResponse>('/api/status')),
  getDashboardVersion: () => get<DashboardVersionResponse>('/api/dashboard/version'),
  getConfig: async () => normalizeConfig(await get<RawConfigResponse>('/api/config')),
  getDonationConfig: async () => {
    try {
      return normalizeDonationConfig(await get<DonationConfig | DonationConfigResponse>('/api/config/donation'));
    } catch (err) {
      if (err instanceof ApiError && err.status !== 404) {
        throw err;
      }
      const cfg = normalizeConfig(await get<RawConfigResponse>('/api/config'));
      return normalizeDonationConfig(cfg.donation);
    }
  },
  /**
   * W9.5: Fetch the public donation pool disclosure (URL + payout
   * address + explorer link). Unauthenticated. Falls through to null
   * on older daemons that don't expose the route, so the dashboard can
   * gracefully degrade rather than throw.
   */
  getDonationInfo: async (): Promise<DonationInfoResponse | null> => {
    try {
      return await get<DonationInfoResponse>('/api/donation/info');
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        return null;
      }
      throw err;
    }
  },
  updateDonationConfig: async (donation: DonationConfig): Promise<DonationConfigResponse> => {
    const normalized = normalizeDonationConfig(donation);
    try {
      const response = await post<DonationConfigResponse>('/api/config/donation', normalized);
      return {
        ...response,
        config: normalizeDonationConfig(response),
      };
    } catch (err) {
      if (err instanceof ApiError && err.status !== 404) {
        throw err;
      }
      const response = await post<ConfigUpdateResponse>('/api/config', { donation: normalized });
      return {
        ...response,
        config: normalized,
        restart_required: true,
      };
    }
  },
  getSetupStatus: (timeoutMs?: number) => get<SetupStatusResponse>('/api/setup/status', timeoutMs),
  setupSafety: () => post<unknown>('/api/setup/step1-safety'),
  setupCircuit: (circuit: SetupCircuitRequest) => post<unknown>('/api/setup/step2-circuit', circuit),
  setupMode: (mode: OperatingMode, hostname?: string) =>
    post<unknown>('/api/setup/step4-mode', {
      mode: toApiMode(mode),
      ...(hostname ? { hostname } : {}),
    }),
  setupPool: (pool: PoolConfigRequest) => post<unknown>('/api/setup/step5-pool', pool),
  // P2-4 (§4.E): persist the operator-confirmed electricity rate + currency to
  // the daemon `[home]` config (single source of truth) and flip
  // electricity_rate_calibrated. Surfaced back via /api/home/status.
  setupEconomics: (req: SetupEconomicsRequest) =>
    post<{ status: string; persisted: boolean; electricity_rate: number; currency: string; electricity_rate_calibrated: boolean }>(
      '/api/setup/step-economics',
      req,
    ),
  // P2-4 (§4.E): quiet-hours captured at setup. Routes through the setup
  // namespace (same handler as /api/home/night-mode) so it is allowed by the
  // pre-device-ready auth gate during the wizard. Clamps fan PWM to the home
  // safety ceiling daemon-side.
  setupQuietHours: (req: NightModeRequest) =>
    post<NightModeResponse>('/api/setup/quiet-hours', req),
  // Freedom-first: operator explicitly declines an owner password. Backend
  // flips password_opt_out so onboarding can complete without one. Write /
  // control endpoints stay locked until a password is set.
  skipPassword: () => post<ActionResponse>('/api/setup/skip-password'),
  // Freedom-first (exact parallel of skipPassword): operator explicitly
  // declines the circuit/breaker/safety acknowledgement. Backend flips
  // safety_opt_out so onboarding can complete without it. This replaces
  // the old silent api.setupSafety() auto-ack on the skip path — that
  // marked the circuit check "done" when the operator did NOT do it.
  // Access is unchanged (dashboard + logs were already reachable on a
  // passwordless unit); a dismissible "circuit check not done" advisory
  // shows until the operator completes it in Settings.
  skipSafety: () => post<ActionResponse>('/api/setup/skip-safety'),
  // Creates the Argon2id owner credential on the backend (POST
  // /api/auth/setup). Mirrors the wizard's call so Settings can ACTUALLY
  // set a password (the old Settings flow only touched the local store and
  // never created the credential — write endpoints stayed locked). Returns
  // the issued session token (or null if none was issued).
  configureAuthPassword: async (password: string): Promise<string | null> => {
    const res = await fetch(`${BASE}/api/auth/setup`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ password }),
    });
    if (!res.ok && res.status !== 409) {
      throw await parseApiError(res);
    }
    if (res.ok) {
      const data = await res.json().catch(() => null);
      return (
        data?.session_token ||
        data?.api_token ||
        data?.session?.session_token ||
        null
      );
    }
    return null;
  },
  completeSetup: () => post<ActionResponse>('/api/setup/complete'),
  updateConfig: (cfg: ConfigUpdateRequest) => post<ConfigUpdateResponse>('/api/config', toApiConfigUpdate(cfg)),
  getSystemInfo: async () => normalizeSystemInfo(await get<RawSystemInfoResponse>('/api/system/info')),
  getDeviceCapability: () => get<DeviceCapabilityDescriptor>('/api/v1/capabilities'),
  getConfigBackupManifest: () => get<ConfigBackupManifestResponse>('/api/config/backup/manifest'),
  // COMP-1 daemon config backup/restore. Export returns the full effective
  // config as a re-importable TOML document with every secret/wallet/credential
  // URL redacted. Import validates (fail-closed) then atomically persists it;
  // placeholder values are treated as keep-existing so a round-trip never
  // overwrites a stored secret with the mask. A 400 surfaces the validation
  // error verbatim (thrown as ApiError so callers can show it honestly).
  getConfigExport: () => get<ConfigExportResponse>('/api/config/export'),
  importConfig: (configToml: string) =>
    post<ConfigImportResponse>('/api/config/import', { config_toml: configToml }),
  getApiCompatibilityManifest: () => get<ApiCompatibilityManifestResponse>('/api/system/api-compatibility/manifest'),
  getCompetitiveReadiness: () => get<CompetitiveReadinessResponse>('/api/competitive/readiness'),
  getNetworkBlock: () => get<NetworkBlockResponse>('/api/network/block'),
  getSystemStats: () => get<SystemStatsResponse>('/api/system/stats'),
  getSystemUpgradeStatus: () => get<SystemUpgradeStatusResponse>('/api/system/upgrade/status'),
  getSystemHealth: async (): Promise<SystemHealthResponse | null> => {
    try {
      return await get<SystemHealthResponse>('/api/system/health');
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        return null;
      }
      throw err;
    }
  },
  getSystemAsic: () => get<SystemAsicResponse>('/api/system/asic'),

  // ──  HIGH-1/2/3 — .25-class XIL  handoff surfaces ──
  // All read-only; dev-firmware no-auth posture per Gate-1 Q3. Each
  // tolerates older daemons (returns null on 404/501) so non-.25 units
  // and pre- builds render the components gracefully empty.
  getEnvRecipe: async (): Promise<import('./types').EnvRecipeResponse | null> => {
    try {
      return await get<import('./types').EnvRecipeResponse>('/api/env/recipe');
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        return null;
      }
      throw err;
    }
  },
  getChainPresence: async (): Promise<import('./types').ChainPresenceResponse | null> => {
    try {
      return await get<import('./types').ChainPresenceResponse>('/api/mining/chain/presence');
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        return null;
      }
      throw err;
    }
  },
  getHandoffState: async (): Promise<import('./types').HandoffStateResponse | null> => {
    try {
      return await get<import('./types').HandoffStateResponse>('/api/mining/handoff/state');
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        return null;
      }
      throw err;
    }
  },
  getSystemBootTimeline: () => get<SystemBootTimelineResponse>('/api/system/boot_timeline'),
  getHardwarePicInfo: () => get<HardwarePicInfoResponse>('/api/hardware/pic_info'),
  getPsuCatalog: () => get<PsuCatalogResponse>('/api/hardware/psu_catalog'),
  getCgminerCatalog: () => get<CgminerCatalogResponse>('/api/cgminer/catalog'),
  getReCatalogIndex: () => get<ReCatalogIndexResponse>('/api/re/catalog/index'),

  // Pools
  getPools: () => get<PoolsResponse>('/api/pools'),
  configurePools: (pool: PoolConfigRequest) => post<PoolConfigResponse>('/api/pools', pool),
  testPoolConnection: (pool: PoolConfigRequest) => post<PoolTestResponse>('/api/pools/test', pool),
  testSetupPoolConnection: (pool: PoolConfigRequest) => post<PoolTestResponse>('/api/setup/test-pool', pool),

  // Stats & History (Standard + Hacker)
  getStats: () => get<StatsResponse>('/api/stats'),
  getRollingMetrics: async (): Promise<RollingMetricsResponse | null> => {
    try {
      return await get<RollingMetricsResponse>('/api/metrics/rolling');
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        return null;
      }
      throw err;
    }
  },
  getThermalPowerPosture: () => get<ThermalPowerPostureResponse>('/api/thermal/posture'),
  getMiningWorkPosture: () => get<MiningWorkPostureResponse>('/api/mining/work/posture'),
  getMiningPipelineManifest: () => get<MiningPipelineManifestResponse>('/api/mining/pipeline/manifest'),
  getMiningPipelineSnapshot: () => get<MiningPipelineSnapshot>('/api/mining/pipeline/snapshot'),
  getMiningPipelineSnapshotSchema: () => get<MiningPipelineSnapshotSchemaResponse>('/api/mining/pipeline/snapshot/schema'),
  saveTouSchedule: (schedule: unknown) => post<ActionResponse>('/api/tou/schedule', schedule),
  getHistory: async () => normalizeHistory(await get<HistoryResponse>('/api/history')),
  getShareHistory: async () => normalizeShareHistory(await get<{ events?: RawRecentShareEvent[] }>('/api/history/shares')),
  getHistoryAudit: (limit = 64) => get<HistoryAuditResponse>(`/api/history/audit?limit=${encodeURIComponent(String(limit))}`),
  getProfiles: () => get<ProfilesResponse>('/api/profiles'),
  saveProfile: (profile: unknown) => post<ActionResponse>('/api/profiles', profile),
  // RE-010 per-chip telemetry. Returns the honest ChipHealthSnapshot
  // (chains[].chipmap.cells[]) — the real source for ChipHeatMap, replacing
  // the old sine-wave fabrication. Optional `chain` scopes to one chain
  // (?chain=N); omit for all chains. Degrades gracefully (returns null) on
  // older daemons that don't expose /api/chips so the UI shows an honest
  // "per-chip telemetry unavailable" state rather than fabricating data.
  getChips: async (chain?: number): Promise<ChipHealthSnapshotResponse | null> => {
    try {
      return await get<ChipHealthSnapshotResponse>(
        `/api/chips${chain != null ? `?chain=${encodeURIComponent(String(chain))}` : ''}`,
      );
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        return null;
      }
      throw err;
    }
  },
  getAutotunerStatus: () => get<AutotunerStatusResponse>('/api/autotuner/status'),
  getAutotunerChipHealth: () => get<AutotunerChipHealthResponse>('/api/autotuner/chip-health'),
  getAutotunerVisibility: () => get<AutotunerVisibilityResponse>('/api/autotuner/visibility'),
  // Full silicon-quality analytics (W9/W15). PURE TELEMETRY — when the report
  // is `characterized: false` the autotuner has not measured these chips yet.
  getAutotunerSiliconReport: () => get<SiliconReportResponse>('/api/autotuner/silicon-report'),
  // Read-only thermal supervisor snapshot (Wave-G). Carries the DIAGNOSTIC
  // chip-imbalance telemetry (inter-chip temp spread; never a control input).
  getThermalSupervisor: () => get<ThermalSupervisorSnapshot>('/api/thermal/supervisor'),
  // Persistent, redacted, newest-first audit log (W10). Survives reboots.
  getAuditLog: (offset = 0, limit = 50) =>
    get<AuditLogResponse>(`/api/audit-log?offset=${offset}&limit=${limit}`),

  // Heater
  getHeaterStatus: () => get<HeaterStatusResponse>('/api/home/status'),
  setHeaterTarget: (target: HeaterTargetRequest) => post<HeaterTargetResponse>('/api/home/target', target),
  getHeaterPresets: () => get<HeaterPresetsResponse>('/api/home/presets'),
  setRoomTemp: (req: RoomTempRequest) => post<ActionResponse>('/api/home/room-temp', req),
  getNightMode: () => get<NightModeResponse>('/api/home/night-mode'),
  setNightMode: (req: NightModeRequest) => post<ActionResponse>('/api/home/night-mode', req),
  getHeaterHistory: async () => normalizeHistory(await get<HistoryResponse>('/api/home/history')),

  // Fan control
  setFan: (mode: string, target_pwm?: number) =>
    post<ActionResponse>('/api/fan', { mode, ...(target_pwm != null ? { target_pwm } : {}) }),

  // Power calibration
  getPowerCalibration: () => get<PowerCalibrationResponse>('/api/config/power-calibration'),
  updatePowerCalibration: (req: PowerCalibrationRequest) => post<PowerCalibrationResponse>('/api/config/power-calibration', req),
  getPsuOverride: () => get<PsuOverrideResponse>('/api/config/psu-override'),
  updatePsuOverride: (req: PsuOverrideRequest) => post<{ status: string; message?: string }>('/api/config/psu-override', req),
  getMqttConfig: () => get<MqttConfig>('/api/config/mqtt'),
  updateMqttConfig: (cfg: MqttConfig) => post<{ status: string; message: string; config?: MqttConfig }>('/api/config/mqtt', cfg),
  testMqttConfig: (cfg: MqttConfig) => post<MqttTestResponse>('/api/config/mqtt/test', cfg),
  // Read-only live publisher health. Degrades gracefully (returns null) on
  // older daemons that don't expose the route so the status card can show an
  // honest "unavailable" state rather than throwing.
  getMqttStatus: async (): Promise<MqttStatusResponse | null> => {
    try {
      return await get<MqttStatusResponse>('/api/mqtt/status');
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        return null;
      }
      throw err;
    }
  },
  discoverFleet: (req: FleetDiscoverRequest) => post<FleetDiscoverResponse>('/api/fleet/discover', req),
  getWebhookConfig: () => get<WebhookConfig>('/api/config/webhook'),
  updateWebhookConfig: (cfg: WebhookConfigUpdateRequest) => post<WebhookConfigUpdateResponse>('/api/config/webhook', cfg),
  testWebhookConfig: (cfg: WebhookConfigUpdateRequest) => post<WebhookTestResponse>('/api/config/webhook/test', cfg),

  // Actions
  restart: () => post<ActionResponse>('/api/action/restart'),
  reboot: () => post<ActionResponse>('/api/action/reboot'),
  sleep: () => post<ActionResponse>('/api/action/sleep'),
  wake: () => post<ActionResponse>('/api/action/wake'),

  // Debug (Hacker only)
  readRegisters: (chain: number, offset: string, count?: number) =>
    get<RegisterReadResponse>(`/api/debug/registers?chain=${chain}&offset=${offset}${count ? `&count=${count}` : ''}`),
  getDebugLog: (lines = 50) => get<{ lines?: string[]; log?: string[] }>(`/api/debug/log?lines=${lines}`),
  writeRegister: (req: RegisterWriteRequest) => post<ActionResponse>('/api/debug/registers', req),
  readI2c: (bus: number, addr: string, reg?: string) =>
    get<I2cReadResponse>(`/api/debug/i2c?bus=${bus}&addr=${addr}${reg ? `&reg=${reg}` : ''}`),
  writeI2c: (req: I2cWriteRequest) => post<ActionResponse>('/api/debug/i2c', req),
  sendAsicCommand: (req: AsicCommandRequest) => post<AsicCommandResponse>('/api/debug/asic-command', req),
  getPidState: () => get<PidStateResponse>('/api/debug/pid-state'),
  setPidParams: (req: PidParamsRequest) => post<ActionResponse>('/api/debug/pid-params', req),
  setChipFrequency: (req: ChipFrequencyRequest) => post<ActionResponse>('/api/debug/chip/frequency', req),
  setChipVoltage: (req: ChipVoltageRequest) => post<ChipVoltageResponse>('/api/debug/chip/voltage', req),

  // Diagnostics
  startHashReport: (req?: DiagnosticStartRequest) => post<DiagnosticStartResponse>('/api/diagnostics/hashreport/start', req),
  cancelHashReport: (testId: string) => post<ActionResponse>('/api/diagnostics/hashreport/cancel', { test_id: testId }),
  getHashReportStatus: (id: string) => get<DiagnosticStatusResponse>(`/api/diagnostics/hashreport/status?test_id=${id}`),
  getHashReportResult: (id: string) => get<DiagnosticResultResponse>(`/api/diagnostics/hashreport/result?test_id=${id}`),
  startChipHealth: (req?: DiagnosticStartRequest) => post<DiagnosticStartResponse>('/api/diagnostics/chip-health/start', req),
  getChipHealthStatus: (id: string) => get<DiagnosticStatusResponse>(`/api/diagnostics/chip-health/status?test_id=${id}`),
  getChipHealthResult: (id: string) => get<DiagnosticResultResponse>(`/api/diagnostics/chip-health/result?test_id=${id}`),
  startBoardHealth: (req?: DiagnosticStartRequest) => post<DiagnosticStartResponse>('/api/diagnostics/board-health/start', req),
  getBoardHealthStatus: (id: string) => get<DiagnosticStatusResponse>(`/api/diagnostics/board-health/status?test_id=${id}`),
  getBoardHealthResult: (id: string) => get<DiagnosticResultResponse>(`/api/diagnostics/board-health/result?test_id=${id}`),
  getRecentDiagnosticReports: (limit = 10) => get<RecentDiagnosticReportsResponse>(`/api/diagnostics/reports/recent?limit=${limit}`),
  getLogManifest: () => get<LogManifestResponse>('/api/diagnostics/logs/manifest'),
  getDiagnosticFailureModes: () => get<DiagnosticsFailureModesResponse>('/api/diagnostics/failure_modes'),
  getChainDiagnostics: (id: number) => get<ChainDiagnosticsResponse>(`/api/diagnostics/chain?id=${encodeURIComponent(String(id))}`),
  getLocalRejects: (limit?: number) => get<LocalRejectsResponse>(
    `/api/diagnostics/shares/local_rejects${limit == null ? '' : `?limit=${encodeURIComponent(String(limit))}`}`,
  ),
  getRecoveryActions: () => get<RecoveryActionsResponse>('/api/diagnostics/recovery_actions'),
  troubleshootNetwork: () => get<NetworkTroubleshootResponse>('/api/diagnostics/troubleshoot/network'),
  troubleshootPsu: () => get<PsuTroubleshootResponse>('/api/diagnostics/troubleshoot/psu'),
  troubleshootFpga: () => get<FpgaTroubleshootResponse>('/api/diagnostics/troubleshoot/fpga'),
  controlPsu: (req: PsuControlRequest) => post<PsuControlResponse>('/api/debug/psu/control', req),
  getOffGridConfig: () => get<OffGridConfigResponse>('/api/offgrid/config'),
  updateOffGridConfig: (req: OffGridConfigPayload) => post<OffGridConfigSaveResponse>('/api/offgrid/config', req),
  getOffGridStatus: () => get<OffGridStatusResponse>('/api/offgrid/status'),
  getOffGridPresets: () => get<OffGridPresetsResponse>('/api/offgrid/presets'),
  testOffGridConfig: (req: OffGridConfigPayload) => post<OffGridProbeResponse>('/api/offgrid/test', req),
  getSolarConfig: async () => normalizeSolarConfig(await get<SolarConfig>('/api/solar/config')),
  updateSolarConfig: async (req: SolarConfig) => {
    const response = await post<{ status: string; message: string; config?: SolarConfig }>('/api/solar/config', toApiSolarConfig(req));
    return {
      ...response,
      config: response.config ? normalizeSolarConfig(response.config) : undefined,
    };
  },
  getSolarStatus: async () => normalizeSolarStatus(await get<SolarStatus>('/api/solar/status')),
  getSolarVerificationHistory: async () => normalizeSolarVerificationHistory(await get<SolarVerificationHistoryResponse>('/api/solar/verification-history')),
  testSolarConfig: async (req: SolarConfig) => normalizeSolarTestResponse(await post<SolarTestResponse>('/api/solar/test', toApiSolarConfig(req))),

  // W11.12 — stock-CGI parity (RE2 §15.2 + competing-firmware features).
  // All read-only; degrade gracefully (return null) on older daemons.
  getNetworkInfo: async (): Promise<NetworkInfoResponse | null> => {
    try { return await get<NetworkInfoResponse>('/api/network/info'); }
    catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) return null;
      throw err;
    }
  },
  updateNetworkHostname: async (hostname: string): Promise<NetworkHostnameResponse> => {
    try {
      return await post<NetworkHostnameResponse>('/api/network/hostname', { hostname });
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        await post('/api/config/shared', { network: { hostname } });
        return {
          status: 'ok',
          persisted: true,
          hostname,
          note: 'Saved through the legacy shared-config route. The active OS hostname updates after the next daemon or host restart.',
        };
      }
      throw err;
    }
  },
  getMinerType: async (): Promise<MinerTypeResponse | null> => {
    try { return await get<MinerTypeResponse>('/api/miner/type'); }
    catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) return null;
      throw err;
    }
  },
  // W13.D1 — PVT table (per-SKU freq/voltage envelope).
  getPvtTable: async (): Promise<PvtTableResponse | null> => {
    try { return await get<PvtTableResponse>('/api/miner/pvt-table'); }
    catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) return null;
      throw err;
    }
  },
  // W13.D1 — Boot-phase tracker (CV1835 6-substate or generic 3-substate).
  getBootPhase: async (): Promise<BootPhaseResponse | null> => {
    try { return await get<BootPhaseResponse>('/api/boot/phase'); }
    catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) return null;
      throw err;
    }
  },
  // W13.D1 — Boot timeline (dev-mode only on backend; null when disabled).
  getBootTimeline: async (): Promise<BootTimelineResponse | null> => {
    try { return await get<BootTimelineResponse>('/api/boot/timeline'); }
    catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) return null;
      throw err;
    }
  },
  // GET /api/log/backup returns text/plain (not JSON); use raw fetch so the
  // dashboard can offer a "Download support bundle" button without parsing.
  fetchLogBackup: async (): Promise<{ text: string; filename: string } | null> => {
    try {
      const res = await request('/api/log/backup');
      if (!res.ok) {
        if (res.status === 404 || res.status === 501) return null;
        throw await parseApiError(res);
      }
      const text = await res.text();
      const cd = res.headers.get('content-disposition') || '';
      const m = /filename="([^"]+)"/.exec(cd);
      const filename = m?.[1] || `dcentos-log-bundle-${Date.now()}.txt`;
      return { text, filename };
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) return null;
      throw err;
    }
  },

  // LED control
  getLedStatus: () => get<LedStatusResponse>('/api/led/status'),
  getLedPatterns: () => get<LedPatternsResponse>('/api/led/patterns'),
  triggerLocate: (req?: LocateRequest) => post<ActionResponse>('/api/led/locate', req || {}),
  stopLocate: () => post<ActionResponse>('/api/led/locate/stop'),
  getLedConfig: () => get<LedConfigResponse>('/api/led/config'),
  updateLedConfig: (cfg: LedConfigUpdateRequest) => post<ActionResponse>('/api/led/config', cfg),

  // SV2 protocol endpoints
  getSv2Status: () => get<Sv2StatusResponse>('/api/pool/sv2/status'),
  getSv2Handshake: () => get<Sv2HandshakeResponse>('/api/pool/sv2/handshake'),
  getSv2Messages: () => get<Sv2MessagesResponse>('/api/pool/sv2/messages'),

  // Job Declaration endpoints
  getJdStatus: () => get<JobDeclarationStatus>('/api/jd/status'),
  postJdConfig: (cfg: JobDeclarationConfig) => post<{status: string, restart_required?: boolean}>('/api/jd/config', cfg),
  testJdConnection: (cfg?: JobDeclarationConfig) => post<{status: string, message: string, checks?: Array<{ok?: boolean}>}>('/api/jd/test-connection', cfg ?? {}),

  // System upgrade
  uploadFirmware: async (
    options: { file?: File; stagedPath?: string; apply?: boolean; onProgress?: (pct: number) => void }
  ): Promise<FirmwareUploadResponse> => {
    const authHeaders = await getAuthHeaders();
    const formData = new FormData();

    if (options.file) {
      formData.append('firmware', options.file);
    }
    if (options.stagedPath) {
      formData.append('staged_path', options.stagedPath);
    }
    if (!options.file && !options.stagedPath) {
      throw new Error('No firmware package was selected');
    }

    formData.append('apply', options?.apply ? 'true' : 'false');
    const xhr = new XMLHttpRequest();
    return new Promise((resolve, reject) => {
      xhr.upload.onprogress = (e) => {
        if (e.lengthComputable && options?.onProgress) options.onProgress((e.loaded / e.total) * 100);
      };
      xhr.onload = () => {
        let payload: unknown = null;
        try {
          payload = xhr.responseText ? JSON.parse(xhr.responseText) : null;
        } catch {
          payload = null;
        }

        if (xhr.status >= 200 && xhr.status < 300) {
          resolve((payload ?? {
            status: 'transport_accepted',
            message: 'Firmware package accepted at transport level; staging or scheduling proof was not returned.',
          }) as FirmwareUploadResponse);
          return;
        }

        const parsed = parseApiErrorText(xhr.status, xhr.responseText || 'Upload failed');
        reject(parsed);
      };
      xhr.onerror = () => reject(new Error('Upload failed'));
      xhr.open('POST', `${BASE}/api/system/upgrade`);
      Object.entries(authHeaders).forEach(([key, value]) => {
        xhr.setRequestHeader(key, value);
      });
      xhr.send(formData);
    });
  },

  // ─── W9.4 — J/TH calibration loop ─────────────────────────────────
  getPerfEfficiency: () => get<PerfEfficiencyResponse>('/api/perf/efficiency'),
  postPerfCalibrate: (body: PerfCalibrateRequest) =>
    post<PerfCalibrateResponse>('/api/perf/calibrate', body),
};

// ─── W9.4 — J/TH calibration types ───────────────────────────────────
export type PerfEfficiencySource = 'operator' | 'pmbus' | 'model';
export type PerfEfficiencyConfidence = 'high' | 'medium' | 'low';

export interface PerfEfficiencyResponse {
  j_per_th: number | null;
  source: PerfEfficiencySource;
  confidence: PerfEfficiencyConfidence;
  measured_at_ms: number | null;
  operator_wall_watts?: number | null;
  operator_hashrate_ths?: number | null;
  jth_target_active: boolean;
}

export interface PerfCalibrateRequest {
  measured_wall_watts?: number;
  hashrate_ghs?: number;
  enabled?: boolean;
}

export interface PerfCalibrateResponse {
  status: 'ok' | 'error';
  message?: string;
  enabled?: boolean;
  operator_confirmed?: boolean;
  measured_wall_watts?: number;
  hashrate_ths?: number;
  j_per_th?: number;
  multiplier?: number;
  measured_at_ms?: number | null;
  source?: PerfEfficiencySource;
}

export { ApiError };
export default api;
