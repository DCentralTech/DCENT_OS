import { apiFetch } from "./client";

export type FleetMinerStatus = "alive" | "dead" | "starting";

export interface FleetMiner {
  id: string;
  hostname: string;
  ip: string;
  model: string;
  hashrate_ghs: number;
  temp_c: number;
  fan_pwm: number;
  status: FleetMinerStatus;
  last_seen_ms: number;
  /**
   * : pool credit / minimum-work evidence — the difficulty the pool
   * asked for (or the Stratum-suggested difficulty for this share). Absent
   * until /api/fleet/miners backend ships the field (PR-048 backend leg).
   */
  pool_target_difficulty?: number | null;
  /**
   * : locally proven achieved difficulty for the last accepted share.
   * This is the ONLY lucky-share evidence.  Distinct from pool_target_difficulty.
   * Absent until /api/fleet/miners backend ships the field (PR-048 backend leg).
   */
  achieved_difficulty?: number | null;
}

export type FleetResponseSource = "api" | "api_unavailable" | "demo";

export interface FleetResponse {
  generated_at_ms: number;
  miners: FleetMiner[];
  source: FleetResponseSource;
  source_label: string;
  demo: boolean;
  message?: string;
}

export interface FleetPoolRollup {
  pool_url: string;
  miner_count: number;
  connected_miners: number;
  donating_miners: number;
  shares_submitted: number;
  shares_accepted: number;
  shares_rejected: number;
  shares_unresolved: number;
  jobs_received: number;
  average_difficulty: number | null;
  acceptance_rate: number | null;
}

export interface FleetPoolMinerSnapshot {
  miner_id: string;
  host: string;
  model?: string | null;
  active_pool_url: string;
  connected: boolean;
  donating: boolean;
  donation_active_url?: string;
  donation_active_worker?: string;
  donation_pool_index?: number;
  shares_submitted: number;
  shares_accepted: number;
  shares_rejected: number;
  shares_unresolved: number;
  pending_submit_dropped: number;
  jobs_received: number;
  current_difficulty: number;
  failover_switch_count: number;
  last_seen_s: number;
}

export interface FleetPoolStats {
  miner_count: number;
  connected_miners: number;
  stale_miners: number;
  donating_miners: number;
  shares_submitted: number;
  shares_accepted: number;
  shares_rejected: number;
  shares_unresolved: number;
  pending_submit_dropped: number;
  jobs_received: number;
  failover_switches: number;
  acceptance_rate: number | null;
  pools: FleetPoolRollup[];
  miners: FleetPoolMinerSnapshot[];
}

export interface FleetPoolStatsResponse {
  schema: string;
  status: string;
  source: string;
  generated_at_s: number;
  stats: FleetPoolStats;
  limitations?: string[];
}

export interface SystemIdentifyResponse {
  message?: string;
  active?: boolean;
}

function emptyUnavailableFleetResponse(message: string): FleetResponse {
  return {
    generated_at_ms: Date.now(),
    source: "api_unavailable",
    source_label: "Fleet API unavailable",
    demo: false,
    message,
    miners: [],
  };
}

function isFleetMinerStatus(value: unknown): value is FleetMinerStatus {
  return value === "alive" || value === "dead" || value === "starting";
}

function assertFleetResponse(value: unknown): FleetResponse {
  if (!value || typeof value !== "object") {
    throw new Error("Fleet inventory response is not an object");
  }

  const response = value as Partial<FleetResponse>;
  if (
    typeof response.generated_at_ms !== "number" ||
    !Array.isArray(response.miners)
  ) {
    throw new Error("Fleet inventory response is missing required fields");
  }

  for (const miner of response.miners) {
    if (!miner || typeof miner !== "object") {
      throw new Error("Fleet inventory row is not an object");
    }

    const row = miner as Partial<FleetMiner>;
    if (
      typeof row.id !== "string" ||
      typeof row.hostname !== "string" ||
      typeof row.ip !== "string" ||
      typeof row.model !== "string" ||
      typeof row.hashrate_ghs !== "number" ||
      typeof row.temp_c !== "number" ||
      typeof row.fan_pwm !== "number" ||
      !isFleetMinerStatus(row.status) ||
      typeof row.last_seen_ms !== "number"
    ) {
      throw new Error("Fleet inventory row is missing required fields");
    }
  }

  return {
    ...(response as FleetResponse),
    source: response.source ?? "api",
    source_label: response.source_label ?? "Local miner API",
    demo: response.demo ?? false,
  };
}

export async function listFleetMiners(): Promise<FleetResponse> {
  try {
    const response = await apiFetch("/api/fleet/miners");
    if (!response.ok) {
      throw new Error(await response.text());
    }
    return assertFleetResponse(await response.json());
  } catch (err) {
    const message = err instanceof Error && err.message.trim()
      ? err.message
      : "Fleet inventory endpoint did not return live/local data.";
    return emptyUnavailableFleetResponse(
      `${message} Demo miners are hidden until explicitly loaded from a demo fixture.`,
    );
  }
}

export async function getFleetPoolStats(): Promise<FleetPoolStatsResponse | null> {
  try {
    const response = await apiFetch("/api/fleet/pool-stats");
    if (!response.ok) return null;
    const body = await response.json() as FleetPoolStatsResponse;
    if (!body || typeof body !== "object" || !body.stats || !Array.isArray(body.stats.miners)) {
      return null;
    }
    return body;
  } catch {
    return null;
  }
}

async function parseIdentifyResponse(response: Response): Promise<SystemIdentifyResponse> {
  if (!response.ok) {
    const detail = await response.text().catch(() => "");
    throw new Error(detail || `Identify failed with status ${response.status}`);
  }
  return response.json().catch(() => ({})) as Promise<SystemIdentifyResponse>;
}

export async function identifyLocalMiner(): Promise<SystemIdentifyResponse> {
  return parseIdentifyResponse(await apiFetch("/api/system/identify", { method: "POST" }));
}

export async function identifyRemoteMiner(ip: string): Promise<SystemIdentifyResponse> {
  const target = new URL("/api/system/identify", `http://${ip}`).toString();
  return parseIdentifyResponse(await fetch(target, { method: "POST" }));
}
