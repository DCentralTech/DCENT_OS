// @vitest-environment jsdom
//
// TEST-DASH-2: the useMinerData data-layer hook owns the critical WS→REST
// fallback, the once-only error-toast debounce, and the heater-mode silent
// stats failure. These were unit-untested (the project's vitest env was
// node-only); this adds a jsdom + @testing-library/react harness and pins the
// highest-risk behaviors with mocked wsManager / api / store.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { cleanup, renderHook } from '@testing-library/react';

const h = vi.hoisted(() => {
  const actions = {
    setWsConnected: vi.fn(),
    setStatus: vi.fn(),
    setStats: vi.fn(),
    addToast: vi.fn(),
    pushHistory: vi.fn(),
    setAutotunerStatus: vi.fn(),
    pushLog: vi.fn(),
    setHeaterStatus: vi.fn(),
    setSystemInfo: vi.fn(),
    setHeaterPresets: vi.fn(),
    setHeaterPresetScope: vi.fn(),
    setNightMode: vi.fn(),
    markWsFrame: vi.fn(),
    markRestPoll: vi.fn(),
    refreshTransportState: vi.fn(),
  };
  const state: Record<string, unknown> = {
    status: null,
    stats: null,
    mode: 'standard',
    heaterStatus: null,
    transport: 'stale',
    ...actions,
  };
  const wsManager = {
    subscribe: vi.fn(() => () => {}),
    onConnectionChange: vi.fn(() => () => {}),
    connect: vi.fn(),
    connected: false,
  };
  const api = {
    getStatus: vi.fn(),
    getStats: vi.fn(),
    getAutotunerStatus: vi.fn(),
    getSystemInfo: vi.fn(),
    getHeaterPresets: vi.fn(),
    getNightMode: vi.fn(),
    getHeaterStatus: vi.fn(),
  };
  return { actions, state, wsManager, api };
});

vi.mock('../api/websocket', () => ({ wsManager: h.wsManager }));
vi.mock('../api/client', () => ({ api: h.api }));
vi.mock('../store/miner', () => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const useMinerStore: any = (sel?: (s: unknown) => unknown) => (sel ? sel(h.state) : h.state);
  useMinerStore.getState = () => h.state;
  return { useMinerStore, useMinerActions: () => h.actions };
});
vi.mock('../utils/power', () => ({ getLiveWallWatts: () => 0 }));

import { useMinerData } from './useMinerData';

const STATUS = {
  hashrate_ghs: 100,
  hashrate_5s_ghs: 100,
  accepted: 0,
  rejected: 0,
  uptime_s: 0,
  firmware_version: 'x',
  mode: 'standard',
  chains: [],
  fans: { pwm: 10, rpm: 0, per_fan: [] },
  pool: { status: 'mining' },
};

const toastMsgs = (substr: string) =>
  h.actions.addToast.mock.calls.filter((c) => String(c[0]).includes(substr));

const flush = () => vi.advanceTimersByTimeAsync(0);
const flushWs = () => vi.advanceTimersByTimeAsync(20);

async function emitWs(message: unknown) {
  const callback = h.wsManager.subscribe.mock.calls[0][0];
  callback(message);
  await flushWs();
}

beforeEach(() => {
  vi.useFakeTimers();
  Object.values(h.actions).forEach((f) => f.mockClear());
  h.wsManager.subscribe.mockClear();
  h.wsManager.onConnectionChange.mockClear();
  h.wsManager.connect.mockClear();
  h.wsManager.connected = false;
  h.state.mode = 'standard';
  h.state.status = null;
  h.state.stats = null;
  h.state.heaterStatus = null;
  h.state.transport = 'stale';
  h.api.getStatus.mockReset().mockResolvedValue(STATUS);
  h.api.getStats.mockReset().mockResolvedValue({ chains: [], power: {} });
  h.api.getAutotunerStatus.mockReset().mockResolvedValue({});
  h.api.getSystemInfo.mockReset().mockResolvedValue({});
  h.api.getHeaterPresets.mockReset().mockResolvedValue({ presets: [] });
  h.api.getNightMode.mockReset().mockResolvedValue({});
  h.api.getHeaterStatus.mockReset().mockResolvedValue({});
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe('useMinerData', () => {
  it('subscribes to the WebSocket and connects on mount', () => {
    renderHook(() => useMinerData());
    expect(h.wsManager.subscribe).toHaveBeenCalled();
    expect(h.wsManager.onConnectionChange).toHaveBeenCalled();
    expect(h.wsManager.connect).toHaveBeenCalled();
  });

  it('REST-polls status when the WebSocket is not connected', async () => {
    h.wsManager.connected = false;
    renderHook(() => useMinerData());
    await flush();
    expect(h.api.getStatus).toHaveBeenCalled();
    expect(h.actions.setStatus).toHaveBeenCalledWith(STATUS);
    expect(h.actions.markRestPoll).toHaveBeenCalled();
  });

  it('normalizes REST status without chain telemetry before storing it', async () => {
    const partialStatus = { ...STATUS };
    delete (partialStatus as { chains?: unknown }).chains;
    h.api.getStatus.mockResolvedValue(partialStatus);

    renderHook(() => useMinerData());
    await flush();

    expect(h.actions.setStatus).toHaveBeenCalledWith(expect.objectContaining({ chains: [] }));
    expect(h.actions.pushHistory).toHaveBeenCalledWith(100, 0, 0);
  });

  it('skips the REST status poll while a WebSocket frame is fresh', async () => {
    h.wsManager.connected = true;
    h.state.transport = 'ws-live';
    renderHook(() => useMinerData());
    await flush();
    expect(h.api.getStatus).not.toHaveBeenCalled();
  });

  it('REST-polls when the socket exists but no recent frame is proven', async () => {
    h.wsManager.connected = true;
    h.state.transport = 'stale';
    renderHook(() => useMinerData());
    await flush();
    expect(h.api.getStatus).toHaveBeenCalled();
    expect(h.actions.markRestPoll).toHaveBeenCalled();
  });

  it('preserves previous chains when a WebSocket stats frame omits chain telemetry', async () => {
    const previousChains = [{
      id: 7,
      chips: 63,
      frequency_mhz: 650,
      voltage_mv: 0,
      temp_c: 52,
      hashrate_ghs: 4000,
      errors: 0,
      status: 'Active',
    }];
    h.wsManager.connected = true;
    h.state.status = { ...STATUS, chains: previousChains };
    h.state.stats = { chains: undefined, power: { watts: 0 } };

    renderHook(() => useMinerData());
    await expect(emitWs({
      type: 'stats',
      hashrate_ghs: 4100,
      hashrate_5s_ghs: 4120,
      accepted: 1,
      rejected: 0,
      fans: { pwm: 20, rpm: 1200 },
      pool: { status: 'mining' },
    })).resolves.toBeUndefined();
    expect(h.actions.setStatus).toHaveBeenCalledWith(expect.objectContaining({ chains: previousChains }));
    expect(h.actions.setStats).toHaveBeenCalledWith(expect.objectContaining({
      chains: [expect.objectContaining({ id: 7, hashrate_ghs: 4000 })],
    }));
  });

  it('shows the status-poll-failure toast only ONCE across repeated failures', async () => {
    h.wsManager.connected = false;
    h.api.getStatus.mockRejectedValue(new Error('down'));
    renderHook(() => useMinerData());
    await flush(); // immediate poll fails → 1 toast
    await vi.advanceTimersByTimeAsync(5000); // 2nd poll fails → debounced, no 2nd toast
    await vi.advanceTimersByTimeAsync(5000); // 3rd poll fails → still debounced
    expect(toastMsgs('Live telemetry unavailable').length).toBe(1);
  });

  it('does NOT toast a stats-poll failure in heater mode (endpoint legitimately absent)', async () => {
    h.state.mode = 'heater';
    h.api.getStats.mockRejectedValue(new Error('no detailed stats endpoint'));
    renderHook(() => useMinerData());
    await flush();
    await vi.advanceTimersByTimeAsync(10000);
    expect(toastMsgs('Detailed stats unavailable').length).toBe(0);
  });

  it('DOES toast a stats-poll failure outside heater mode (once)', async () => {
    h.state.mode = 'standard';
    h.api.getStats.mockRejectedValue(new Error('no stats'));
    renderHook(() => useMinerData());
    await flush();
    await vi.advanceTimersByTimeAsync(10000);
    expect(toastMsgs('Detailed stats unavailable').length).toBe(1);
  });

  it('does not attach stale REST power provenance to heater WebSocket watts', async () => {
    h.state.heaterStatus = {
      power_watts: 1000,
      wall_watts: 1050,
      btu_h: 3582,
      source: 'pmbus',
      power_source_detail: 'pmbus_measured',
      live_power_available: true,
      power_modeled: false,
      noise_db: null,
      airflow_cfm: 0,
      preset: 'balanced',
      room_temp_c: null,
      cost_today_usd: 0,
      sats_today: 0,
      night_mode_active: false,
      night_mode_starts_in_s: null,
      hashrate_ghs: 0,
    };

    renderHook(() => useMinerData());
    await emitWs({
      type: 'heater_status',
      power_watts: 700,
      wall_watts: 800,
      btu_h: 2730,
      noise_db: null,
      airflow_cfm: 0,
      preset: 'quiet',
      room_temp_c: null,
      cost_today_usd: 0,
      sats_today: 0,
      night_mode_active: false,
      night_mode_starts_in_s: null,
    });

    expect(h.actions.setHeaterStatus).toHaveBeenCalledWith(expect.objectContaining({
      power_watts: 700,
      wall_watts: 800,
      btu_h: 2730,
      source: 'static_model_fallback',
      power_source_detail: 'static_power_fallback_from_miner_state',
      live_power_available: false,
      power_modeled: true,
      calibrated: false,
      calibration_multiplier: null,
      power_note: expect.stringContaining('lacks live provenance'),
    }));
  });

  it('passes heater WebSocket power provenance through when the daemon sends it', async () => {
    renderHook(() => useMinerData());

    await emitWs({
      type: 'heater_status',
      power_watts: 740,
      wall_watts: 790,
      btu_h: 2695,
      power_source: 'pmbus',
      power_source_detail: 'pmbus_measured',
      live_power_available: true,
      power_modeled: false,
      power_note: 'Power is sourced from live measured telemetry.',
      power_calibrated: true,
      power_calibration_multiplier: 1.05,
      noise_db: 47,
      noise_source: 'tach_estimate',
      noise_note: 'Estimated from live fan RPM',
      airflow_cfm: 74,
      preset: 'quiet',
      room_temp_c: 21.2,
      cost_today_usd: 0.18,
      sats_today: 88,
      night_mode_active: false,
      night_mode_starts_in_s: null,
    });

    expect(h.actions.setHeaterStatus).toHaveBeenCalledWith(expect.objectContaining({
      power_watts: 740,
      wall_watts: 790,
      btu_h: 2695,
      source: 'pmbus',
      power_source_detail: 'pmbus_measured',
      live_power_available: true,
      power_modeled: false,
      power_note: 'Power is sourced from live measured telemetry.',
      calibrated: true,
      calibration_multiplier: 1.05,
      noise_db: 47,
      noise_source: 'tach_estimate',
    }));
  });
});
