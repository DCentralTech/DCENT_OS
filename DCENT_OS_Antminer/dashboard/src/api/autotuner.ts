// Autotuner control API helpers (W15-B, wave 15).
//
// Wraps the live-runtime endpoints W13-A wired:
//
//   GET  /api/autotuner/status                   live runtime status (rest.rs:15305)
//   POST /api/autotuner/increment_hashrate_target adjust target hashrate up
//   POST /api/autotuner/decrement_hashrate_target adjust target hashrate down
//   POST /api/autotuner/increment_power_target    adjust target power up
//   POST /api/autotuner/decrement_power_target    adjust target power down
//   POST /api/autotuner/set_default_hashrate_target    return to default tuner mode
//
// Profile selection — keyed by (miner_model, hashboard) tuple, NOT chain_id.
// The autotuner's `active_silicon_profile_ids` HashMap uses
// (model, hashboard) as its key (see dcentrald-autotuner/src/tuner.rs:536).
// This is already wrapped by `siliconProfilesApi.setActive` in
// `./profiles-silicon.ts`, which is what this panel uses.
//
// `PUT /api/autotuner/active` is the strict mode-write route on current
// daemons. The helper keeps a 404/405 fallback through generic
// `POST /api/config` so older field daemons still persist the requested
// `autotuner.tuner_mode` without pretending the live runtime accepted it.

import { apiFetch } from './client';
import type { AutotunerStatusResponse, AutotunerTelemetryResponse } from './types';

/**
 * Operator-facing tuner mode the dashboard surfaces. Maps to the
 * `TunerMode` enum in `dcentrald-autotuner/src/config.rs:245`.
 *
 * Wire shape mirrors the serde tag-mode: `{mode: "performance"}` /
 * `{mode: "power_target", watts: 1200}` / `{mode: "hashrate_target",
 * ths: 14}` / `{mode: "manual", freq_mhz: 650, voltage_mv: 9100}`.
 *
 * The dashboard panel only exposes the simple presets without operator
 * arguments (`Performance`, `Efficiency`); the targeted modes live in
 * the existing increment/decrement workflow and are surfaced as
 * step-up / step-down buttons.
 */
export type AutoTunerSimpleMode = 'performance' | 'efficiency';

export type AutoTunerModeRequest =
  | { mode: 'performance' }
  | { mode: 'efficiency' }
  | { mode: 'power_target'; watts: number }
  | { mode: 'hashrate_target'; ths: number }
  | { mode: 'manual'; freq_mhz: number; voltage_mv: number }
  | { mode: 'heater'; btu_h: number };

/**
 * Backend ack vocabulary (`AutoTunerCommandStatus` —
 * `dcentrald-autotuner/src/lib.rs:306`). The autotuner reports whether
 * the runtime command was applied immediately (`applied`), queued for
 * the next iteration (`deferred`), or dropped because it was outside
 * the autotuner's current state (`rejected`).
 *
 * The dashboard surfaces `applied` / `deferred` as success and
 * `rejected` as a warning. The persisted-config write succeeds either
 * way — `rejected` only means the autotuner thread was busy.
 */
export type AutoTunerAckStatus = 'applied' | 'deferred' | 'rejected';

/** Response shape from the increment/decrement/set_default endpoints. */
export interface AutoTunerModeResponse {
  status: string;
  mode: { mode: string; [k: string]: unknown };
  step?: unknown;
  /** Present when the command reached the live runtime channel. */
  runtime?: {
    status: AutoTunerAckStatus;
    applied_runtime: boolean;
    message?: string;
  };
  runtime_command?: AutoTunerModeResponse['runtime'];
  message?: string;
}

async function unpackError(res: Response): Promise<never> {
  let body: unknown = null;
  try { body = await res.json(); } catch { /* ignore */ }
  const reason = (body && typeof body === 'object' && 'message' in body && typeof (body as { message?: unknown }).message === 'string')
    ? (body as { message: string }).message
    : `HTTP ${res.status}`;
  const err = new Error(reason);
  (err as Error & { status?: number }).status = res.status;
  (err as Error & { body?: unknown }).body = body;
  throw err;
}

async function postNoBody<T>(path: string): Promise<T> {
  const res = await apiFetch(path, { method: 'POST' });
  if (!res.ok) await unpackError(res);
  return normalizeModeResponse(await res.json() as AutoTunerModeResponse) as T;
}

function normalizeModeResponse(response: AutoTunerModeResponse): AutoTunerModeResponse {
  return {
    ...response,
    runtime: response.runtime ?? response.runtime_command,
  };
}

async function putActiveMode(mode: AutoTunerModeRequest): Promise<AutoTunerModeResponse> {
  const res = await apiFetch('/api/autotuner/active', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(mode),
  });
  if (res.status === 404 || res.status === 405) {
    return postLegacyConfigMode(mode);
  }
  if (!res.ok) await unpackError(res);
  return normalizeModeResponse(await res.json() as AutoTunerModeResponse);
}

async function postLegacyConfigMode(mode: AutoTunerModeRequest): Promise<AutoTunerModeResponse> {
  const res = await apiFetch('/api/config', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ autotuner: { tuner_mode: mode } }),
  });
  if (!res.ok) await unpackError(res);
  return {
    status: 'ok',
    mode,
    message: 'Autotuner mode saved through the legacy config route. Runtime application requires daemon support.',
  };
}

export const autotunerApi = {
  /**
   * `GET /api/autotuner/status` — live runtime snapshot. Includes
   * mode/policy, per-chain limits, silicon grade counts, and
   * convergence indicators (`active_chips`, `total_chips`,
   * `percent_complete`).
   *
   * Polled every 2s by `<AutoTunerPanel />`.
   */
  getStatus: async (): Promise<AutotunerStatusResponse> => {
    const res = await apiFetch('/api/autotuner/status');
    if (!res.ok) await unpackError(res);
    return res.json();
  },

  getTelemetry: async (): Promise<AutotunerTelemetryResponse> => {
    const res = await apiFetch('/api/autotuner/telemetry');
    if (res.status === 404 || res.status === 501) {
      return {
        live_runtime: false,
        recording: false,
        runs: [],
        last_update_s: 0,
        source: 'route_unavailable',
        stale: true,
        message: 'Autotuner telemetry JSON is not available on this daemon.',
      };
    }
    if (!res.ok) await unpackError(res);
    return res.json();
  },

  setMode: (mode: AutoTunerModeRequest): Promise<AutoTunerModeResponse> =>
    putActiveMode(mode),

  /**
   * Step the active hashrate target up by one walker step (rest.rs:15410).
   * Returns the new mode + runtime ack.
   */
  incrementHashrate: () =>
    postNoBody<AutoTunerModeResponse>('/api/autotuner/increment_hashrate_target'),

  /** Step the active hashrate target down by one walker step. */
  decrementHashrate: () =>
    postNoBody<AutoTunerModeResponse>('/api/autotuner/decrement_hashrate_target'),

  /** Step the active power target up by one walker step. */
  incrementPower: () =>
    postNoBody<AutoTunerModeResponse>('/api/autotuner/increment_power_target'),

  /** Step the active power target down by one walker step. */
  decrementPower: () =>
    postNoBody<AutoTunerModeResponse>('/api/autotuner/decrement_power_target'),

  /**
   * Return to the platform default hashrate target. Used by the panel's
   * "Reset" button.
   */
  setDefaultHashrateTarget: () =>
    postNoBody<AutoTunerModeResponse>('/api/autotuner/set_default_hashrate_target'),

  /**
   * `GET /api/autotuner/telemetry/csv` — export the last tuning run's
   * per-iteration telemetry as a CSV file (W9). Fetched through `apiFetch`
   * so the session auth header is attached, then streamed to a browser
   * download. Returns the suggested filename on success.
   *
   * Honest empty state: when no tuning run has been recorded the backend
   * returns HTTP 404 `{error:"no_runs"}`. This resolves to `null` (NOT a
   * thrown error) so the caller can surface a benign "nothing to export
   * yet" message rather than a failure toast. Returns the filename on a
   * real export.
   */
  downloadTelemetryCsv: async (): Promise<string | null> => {
    const res = await apiFetch('/api/autotuner/telemetry/csv');
    if (res.status === 404) return null;
    if (!res.ok) await unpackError(res);
    const blob = await res.blob();
    const filename = 'autotuner-telemetry.csv';
    const url = URL.createObjectURL(blob);
    try {
      const a = document.createElement('a');
      a.href = url;
      a.download = filename;
      document.body.appendChild(a);
      a.click();
      a.remove();
    } finally {
      // Defer revocation so the navigation/download has a chance to start.
      window.setTimeout(() => URL.revokeObjectURL(url), 4000);
    }
    return filename;
  },
};

export type { AutotunerStatusResponse } from './types';
