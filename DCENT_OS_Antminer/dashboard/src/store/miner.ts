// Zustand store — miner state, mode, config, alerts

import { create } from 'zustand';
import type {
  OperatingMode, StatusResponse, HeaterStatusResponse,
  ChainState, FanState, PoolState, SystemInfoResponse,
  StatsResponse, NightModeResponse, HeaterPreset, WsLogMessage, AutotunerStatusResponse,
  HeaterPresetScope, SetupStatusResponse,
} from '../api/types';
import {
  getSessionToken,
  setSessionToken,
  getVolatilePassword,
  setVolatilePassword,
  stripCredentialFields,
  migrateLegacyCredentials,
} from '../api/credentials';
import { RingBuffer } from './ringBuffer';
import { flashAlert } from '../fx/titleTicker';

// ─── Log Entry (from WebSocket) ─────────────────────────────
export interface LogEntry {
  id: number;
  timestamp: number;
  level: 'info' | 'warn' | 'error' | 'debug';
  source: 'mining' | 'system';
  message: string;
}

// ─── Toast System ───────────────────────────────────────────
export interface Toast {
  id: string;
  message: string;
  type: 'success' | 'error' | 'warning' | 'info';
  createdAt: number;
}

// ─── Alert System ───────────────────────────────────────────
export interface Alert {
  id: string;
  level: 'info' | 'warning' | 'critical';
  message: string;
  timestamp: number;
  dismissed: boolean;
  source?: 'user' | 'health';
  dedupeKey?: string;
}

export interface TaskHandoff {
  fromMode: OperatingMode;
  fromPage: string;
  toMode: OperatingMode;
  toPage: string;
  returnLabel?: string;
}

export type TransportKind = 'ws-live' | 'rest-polling' | 'stale';

const WS_LIVE_WINDOW_MS = 5000;
const REST_POLLING_WINDOW_MS = 15000;

export function deriveTransportKind(
  now: number,
  wsConnected: boolean,
  lastWsFrameAt: number,
  lastRestPollAt: number,
): TransportKind {
  if (wsConnected && lastWsFrameAt > 0 && now - lastWsFrameAt < WS_LIVE_WINDOW_MS) {
    return 'ws-live';
  }
  if (lastRestPollAt > 0 && now - lastRestPollAt < REST_POLLING_WINDOW_MS) {
    return 'rest-polling';
  }
  return 'stale';
}

// ─── Settings (persisted to localStorage) ───────────────────
export interface DashboardSettings {
  electricityRate: number;      // $/kWh — mirrors daemon [home].electricity_rate (single source of truth, P2-4)
  currency: string;             // display currency code (e.g. "USD") — mirrors daemon [home].currency
  // P2-4 (§4.E): true once the operator has CONFIRMED a rate (daemon
  // [home].electricity_rate_calibrated). While false, electricityRate is the
  // daemon default guess and cost/earnings surfaces must be labelled an
  // uncalibrated estimate, never presented as operator-confirmed truth.
  electricityRateCalibrated: boolean;
  btcPrice: number;             // USD (manual; local-first — no browser-side price fetch)
  soundAlerts: boolean;
  browserNotifications: boolean;
  password: string | null;      // owner password — held IN MEMORY only (api/credentials), never persisted at rest
  apiToken: string | null;      // bearer session token — persisted in sessionStorage (api/credentials), not this blob
  minerName: string;
  setupComplete: boolean;
  temperatureUnit: 'C' | 'F';  // Temperature display preference
  powerBudgetWatts: number | null; // Max power budget (null = unlimited)
  btcPriceLastUpdated: number | null; // Timestamp (ms) when BTC price was last changed
  mode: OperatingMode;          // UI mode — persisted locally, NOT overwritten by API
  betaView: boolean;            // Hide internal contract gates + dev telemetry from Standard mode
  // UINAV-7: light/dark surface preference. ORTHOGONAL to `mode` (the
  // basic/standard/advanced industrial skins) — it only changes surface
  // lightness, not which mode you're in. Persisted locally; default 'dark'
  // so existing users + existing persisted blobs render byte-identically.
  appearance: 'dark' | 'light';
}

// ─── Store ──────────────────────────────────────────────────
interface MinerStore {
  // Connection
  wsConnected: boolean;
  transport: TransportKind;
  lastWsFrameAt: number;
  lastRestPollAt: number;
  lastUpdate: number;

  // Core status
  mode: OperatingMode;
  status: StatusResponse | null;
  systemInfo: SystemInfoResponse | null;
  stats: StatsResponse | null;
  autotunerStatus: AutotunerStatusResponse | null;
  // Backend first-boot / onboarding status. Source of truth for the
  // "no owner password" freedom-first security advisory.
  setupStatus: SetupStatusResponse | null;

  // Heater
  heaterStatus: HeaterStatusResponse | null;
  heaterPresets: HeaterPreset[];
  heaterPresetScope: HeaterPresetScope | null;
  nightMode: NightModeResponse | null;

  // UI state
  currentPage: string;
  sidebarCollapsed: boolean;
  alerts: Alert[];
  authenticated: boolean;  // For Advanced mode password gate
  navState: { heater: string; standard: string; hacker: string };  // Per-mode page memory
  taskHandoff: TaskHandoff | null;

  // Settings
  settings: DashboardSettings;

  // History buffer for charts (ring buffer, last 1440 points = 24h at 1/min)
  hashrateHistory: { time: number; value: number }[];
  tempHistory: { time: number; value: number }[];
  powerHistory: { time: number; value: number }[];

  // Log entries from WebSocket (ring buffer, last 500 entries)
  logEntries: LogEntry[];

  // Toast notifications
  toasts: Toast[];

  // Computed
  dataLoaded: boolean;

  // Actions
  setWsConnected: (v: boolean) => void;
  markWsFrame: (at: number) => void;
  markRestPoll: (at: number) => void;
  refreshTransportState: (now: number) => void;
  setMode: (m: OperatingMode) => void;
  setStatus: (s: StatusResponse) => void;
  setSystemInfo: (i: SystemInfoResponse) => void;
  setStats: (s: StatsResponse) => void;
  setSetupStatus: (s: SetupStatusResponse | null) => void;
  setAutotunerStatus: (s: AutotunerStatusResponse) => void;
  setHeaterStatus: (h: HeaterStatusResponse) => void;
  setHeaterPresets: (p: HeaterPreset[]) => void;
  setHeaterPresetScope: (s: HeaterPresetScope | null) => void;
  setNightMode: (n: NightModeResponse) => void;
  setCurrentPage: (p: string) => void;
  toggleSidebar: () => void;
  addAlert: (level: Alert['level'], message: string) => void;
  dismissAlert: (id: string) => void;
  upsertHealthAlert: (alert: { key: string; level: Alert['level']; message: string }) => void;
  clearHealthAlert: (key: string) => void;
  setAuthenticated: (v: boolean) => void;
  setTaskHandoff: (handoff: TaskHandoff | null) => void;
  clearTaskHandoff: () => void;
  updateSettings: (s: Partial<DashboardSettings>) => void;
  pushHistory: (hashrate: number, temp: number, power: number) => void;
  pushLog: (level: LogEntry['level'], source: LogEntry['source'], message: string) => void;
  addToast: (message: string, type: Toast['type']) => void;
  removeToast: (id: string) => void;
}

const DEFAULT_SETTINGS: DashboardSettings = {
  // P2-4 (§4.E): match the daemon default ([home].electricity_rate = 0.12). The
  // old client guess of 0.10 disagreed with the daemon, so cost/earnings math
  // differed between the dashboard and the daemon. The daemon is now the single
  // source of truth and this is only the pre-hydration fallback.
  electricityRate: 0.12,
  currency: 'USD',
  electricityRateCalibrated: false,
  btcPrice: 100000,
  soundAlerts: false,
  browserNotifications: false,
  password: null,
  apiToken: null,
  minerName: 'My Miner',
  setupComplete: false,
  temperatureUnit: 'C',
  powerBudgetWatts: null,
  btcPriceLastUpdated: null,
  mode: 'standard',
  betaView: true,
  // UINAV-7: default to the current dark appearance so existing users and
  // every previously-persisted settings blob (which the ...DEFAULT_SETTINGS
  // spread back-fills) render unchanged. Light is strictly opt-in.
  appearance: 'dark',
};

function loadSettings(): DashboardSettings {
  let persisted: Partial<DashboardSettings> = {};
  try {
    const raw = localStorage.getItem('dcentos-settings');
    if (raw) persisted = JSON.parse(raw) as Partial<DashboardSettings>;
  } catch { /* ignore */ }
  // Credentials live in api/credentials, never the persisted blob. Strip any
  // that slipped in (legacy / hand-edited) and rehydrate from the dedicated
  // session-token + in-memory-password stores.
  return {
    ...DEFAULT_SETTINGS,
    ...stripCredentialFields(persisted),
    apiToken: getSessionToken(),
    password: getVolatilePassword(),
  };
}

function saveSettings(s: DashboardSettings) {
  // Route credentials to their dedicated, isolated stores...
  setSessionToken(s.apiToken);
  setVolatilePassword(s.password);
  // ...and never write them into the durable, broadly-read settings blob.
  localStorage.setItem('dcentos-settings', JSON.stringify(stripCredentialFields(s)));
}

const MAX_HISTORY = 1440;
const MAX_LOG_ENTRIES = 500;

// P3-6: fixed-size circular buffers, allocated once and pushed in place, replace
// the old per-tick `[...arr, x].slice(-cap)` rebuild (two transient allocations
// every telemetry tick → GC churn on the always-open kiosk). The store state
// still mirrors each buffer's ordered snapshot, so the consumer-facing shape
// ({time,value}[] / LogEntry[]) is byte-for-byte unchanged.
type HistoryPoint = { time: number; value: number };
const hashrateBuffer = new RingBuffer<HistoryPoint>(MAX_HISTORY);
const tempBuffer = new RingBuffer<HistoryPoint>(MAX_HISTORY);
const powerBuffer = new RingBuffer<HistoryPoint>(MAX_HISTORY);
const logBuffer = new RingBuffer<LogEntry>(MAX_LOG_ENTRIES);

let alertCounter = 0;
let toastCounter = 0;

const ALERT_LEVEL_PRIORITY: Record<Alert['level'], number> = {
  info: 1,
  warning: 2,
  critical: 3,
};

function triggerAlertEffects(alert: Alert, settings: DashboardSettings) {
  if (settings.browserNotifications && alert.level === 'critical') {
    if (Notification.permission === 'granted') {
      new Notification('DCENT_OS Alert', { body: alert.message, icon: '/favicon.ico' });
    }
  }

  if (settings.soundAlerts && alert.level === 'critical') {
    try {
      const ctx = new AudioContext();
      const osc = ctx.createOscillator();
      osc.frequency.value = 880;
      osc.connect(ctx.destination);
      osc.start();
      setTimeout(() => { osc.stop(); ctx.close(); }, 200);
    } catch { /* ignore */ }
  }

  if (alert.level === 'critical') {
    flashAlert(alert.message);
  }
}

// Compute initial navState and currentPage based on saved mode
function initNavState(): { navState: { heater: string; standard: string; hacker: string }; currentPage: string } {
  const navState = {
    heater: localStorage.getItem('dcentos-nav-heater') || 'heater-home',
    standard: localStorage.getItem('dcentos-nav-standard') || 'dashboard',
    hacker: localStorage.getItem('dcentos-nav-hacker') || 'dashboard',
  };
  const mode = (loadSettings().mode || 'standard') as keyof typeof navState;
  // Use saved currentPage if available, otherwise fall back to the mode's default
  const currentPage = localStorage.getItem('dcentos-current-page') || navState[mode];
  return { navState, currentPage };
}

// Purge any plaintext password / bearer token left in the legacy single-blob
// layout BEFORE the store hydrates from `dcentos-settings`.
migrateLegacyCredentials();

const { navState: initialNavState, currentPage: initialPage } = initNavState();

export const useMinerStore = create<MinerStore>((set, get) => ({
  wsConnected: false,
  transport: 'stale',
  lastWsFrameAt: 0,
  lastRestPollAt: 0,
  lastUpdate: 0,
  mode: loadSettings().mode || 'standard',
  status: null,
  systemInfo: null,
  stats: null,
  autotunerStatus: null,
  setupStatus: null,
  heaterStatus: null,
  heaterPresets: [],
  heaterPresetScope: null,
  nightMode: null,
  currentPage: initialPage,
  sidebarCollapsed: false,
  alerts: [],
  authenticated: false,
  navState: initialNavState,
  taskHandoff: null,
  settings: loadSettings(),
  hashrateHistory: [],
  tempHistory: [],
  powerHistory: [],
  logEntries: [],
  toasts: [],
  dataLoaded: false,

  setWsConnected: (v) => set((s) => ({
    wsConnected: v,
    transport: deriveTransportKind(Date.now(), v, s.lastWsFrameAt, s.lastRestPollAt),
  })),
  markWsFrame: (at) => set((s) => ({
    wsConnected: true,
    lastWsFrameAt: at,
    transport: deriveTransportKind(at, true, at, s.lastRestPollAt),
  })),
  markRestPoll: (at) => set((s) => ({
    lastRestPollAt: at,
    transport: deriveTransportKind(at, s.wsConnected, s.lastWsFrameAt, at),
  })),
  refreshTransportState: (now) => set((s) => {
    const transport = deriveTransportKind(now, s.wsConnected, s.lastWsFrameAt, s.lastRestPollAt);
    return transport === s.transport ? {} : { transport };
  }),
  setMode: (m) => {
    // Save current page to old mode's navState
    const { mode: oldMode, currentPage, navState, settings } = get();
    const updatedNavState = { ...navState, [oldMode]: currentPage };
    // Restore page from new mode's navState
    const restoredPage = updatedNavState[m];
    // Persist mode to localStorage so it survives page refresh and API overwrites
    const nextSettings = { ...settings, mode: m };
    saveSettings(nextSettings);
    localStorage.setItem('dcentos-current-page', restoredPage);
    localStorage.setItem(`dcentos-nav-${oldMode}`, currentPage);
    set({ mode: m, settings: nextSettings, currentPage: restoredPage, navState: updatedNavState });
  },
  // CRITICAL: Do NOT overwrite mode from API response — mode is a client-side UI preference
  setStatus: (s) => set({ status: s, lastUpdate: Date.now(), dataLoaded: true }),
  setSystemInfo: (i) => set({ systemInfo: i }),
  setStats: (s) => set({ stats: s }),
  setSetupStatus: (s) => set({ setupStatus: s }),
  setAutotunerStatus: (s) => set({ autotunerStatus: s }),
  setHeaterStatus: (h) => {
    // P2-4 (§4.E): the daemon [home] config is the single source of truth for
    // the electricity rate + currency. Surface the daemon-reported values into
    // settings so cost/earnings use THEM (not the old localStorage guess) and
    // so the dashboard knows when to flag earnings "uncalibrated". The
    // calibration flag always tracks the daemon; the confirmed rate/currency is
    // adopted once (the first time the daemon reports calibrated) so a later
    // local edit is not clobbered by every poll.
    const { settings } = get();
    const patch: Partial<DashboardSettings> = {};
    if (typeof h.electricity_rate_calibrated === 'boolean'
        && settings.electricityRateCalibrated !== h.electricity_rate_calibrated) {
      patch.electricityRateCalibrated = h.electricity_rate_calibrated;
    }
    if (h.electricity_rate_calibrated === true && settings.electricityRateCalibrated !== true) {
      if (typeof h.electricity_rate === 'number' && h.electricity_rate > 0
          && h.electricity_rate !== settings.electricityRate) {
        patch.electricityRate = h.electricity_rate;
      }
      if (typeof h.currency === 'string' && h.currency.trim() && h.currency !== settings.currency) {
        patch.currency = h.currency;
      }
    }
    if (Object.keys(patch).length > 0) {
      const next = { ...settings, ...patch };
      saveSettings(next);
      set({ heaterStatus: h, settings: next });
    } else {
      set({ heaterStatus: h });
    }
  },
  setHeaterPresets: (p) => set({ heaterPresets: p }),
  setHeaterPresetScope: (s) => set({ heaterPresetScope: s }),
  setNightMode: (n) => set({ nightMode: n }),
  setCurrentPage: (p) => {
    const { mode, navState } = get();
    const updatedNavState = { ...navState, [mode]: p };
    localStorage.setItem('dcentos-current-page', p);
    localStorage.setItem(`dcentos-nav-${mode}`, p);
    set({ currentPage: p, navState: updatedNavState });
  },
  toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
  addAlert: (level, message) => {
    const id = `alert-${++alertCounter}`;
    const alert: Alert = { id, level, message, timestamp: Date.now(), dismissed: false, source: 'user' };
    set((s) => ({ alerts: [...s.alerts.slice(-49), alert] }));
    triggerAlertEffects(alert, get().settings);
  },
  dismissAlert: (id) => set((s) => ({
    alerts: s.alerts.map(a => a.id === id ? { ...a, dismissed: true } : a),
  })),
  upsertHealthAlert: ({ key, level, message }) => {
    let alertToNotify: Alert | null = null;

    set((s) => {
      const existingIndex = s.alerts.findIndex(a => a.source === 'health' && a.dedupeKey === key);
      if (existingIndex === -1) {
        const alert: Alert = {
          id: `alert-${++alertCounter}`,
          level,
          message,
          timestamp: Date.now(),
          dismissed: false,
          source: 'health',
          dedupeKey: key,
        };
        alertToNotify = alert;
        return { alerts: [...s.alerts.slice(-49), alert] };
      }

      const existing = s.alerts[existingIndex];
      const existingPriority = ALERT_LEVEL_PRIORITY[existing.level];
      const nextPriority = ALERT_LEVEL_PRIORITY[level];
      const escalated = nextPriority > existingPriority;
      const messageChanged = existing.message !== message;

      if (!escalated && !messageChanged) {
        return s;
      }

      const nextAlert: Alert = {
        ...existing,
        level,
        message,
        timestamp: Date.now(),
        dismissed: escalated ? false : existing.dismissed,
      };

      if (escalated) {
        alertToNotify = nextAlert;
      }

      return {
        alerts: s.alerts.map((alert, index) => index === existingIndex ? nextAlert : alert),
      };
    });

    if (alertToNotify) {
      triggerAlertEffects(alertToNotify, get().settings);
    }
  },
  clearHealthAlert: (key) => set((s) => ({
    alerts: s.alerts.filter(alert => !(alert.source === 'health' && alert.dedupeKey === key)),
  })),
  setAuthenticated: (v) => set({ authenticated: v }),
  setTaskHandoff: (taskHandoff) => set({ taskHandoff }),
  clearTaskHandoff: () => set({ taskHandoff: null }),
  updateSettings: (partial) => {
    const next = { ...get().settings, ...partial };
    saveSettings(next);
    set({ settings: next });
  },
  pushHistory: (hashrate, temp, power) => {
    const time = Date.now() / 1000;
    hashrateBuffer.push({ time, value: hashrate });
    tempBuffer.push({ time, value: temp });
    powerBuffer.push({ time, value: power });
    set({
      hashrateHistory: hashrateBuffer.toArray(),
      tempHistory: tempBuffer.toArray(),
      powerHistory: powerBuffer.toArray(),
    });
  },
  pushLog: (level, source, message) => {
    logBuffer.push({
      id: Date.now() + Math.random(),
      timestamp: Date.now(),
      level,
      source,
      message,
    });
    set({ logEntries: logBuffer.toArray() });
  },
  addToast: (message, type) => {
    const id = `toast-${++toastCounter}`;
    const toast: Toast = { id, message, type, createdAt: Date.now() };
    set((s) => ({ toasts: [...s.toasts.slice(-9), toast] }));
  },
  removeToast: (id) => set((s) => ({
    toasts: s.toasts.filter(t => t.id !== id),
  })),
}));

/**
 * Selector hook for the Standard-mode "beta view" preference.
 * Beta view defaults to ON for new installs — it hides internal
 * contract gates (CompetitiveReadiness, MiningPipelineManifest,
 * HonestMode banners/cards, the Production Hardening release notice)
 * so beta testers get a calm operator-facing dashboard. Power users
 * turn it off via Settings → General → Beta View.
 */
export function useBetaView(): boolean {
  return useMinerStore(s => s.settings.betaView !== false);
}

/**
 * Selector hook returning ONLY the store's action functions.
 *
 * P3-6: `useMinerData` previously did a bare `useMinerStore()` with no selector,
 * which subscribes the host component (App) to the ENTIRE store — so App and its
 * subtree re-rendered on every telemetry tick (setStatus / setStats /
 * pushHistory / pushLog all fire each tick). The hook only ever needs the
 * (stable) action functions inside effects, never reactive state for rendering.
 *
 * Each action is selected individually: every Zustand action keeps a stable
 * identity for the store's lifetime, so these subscriptions compare equal on
 * every state change and never trigger a re-render. (The returned object is a
 * fresh literal per call, which is fine — the caller reads it inside `[]`-dep
 * effects, not as a render dependency.)
 */
export function useMinerActions() {
  const setWsConnected = useMinerStore(s => s.setWsConnected);
  const setStatus = useMinerStore(s => s.setStatus);
  const setStats = useMinerStore(s => s.setStats);
  const setSystemInfo = useMinerStore(s => s.setSystemInfo);
  const setAutotunerStatus = useMinerStore(s => s.setAutotunerStatus);
  const setHeaterStatus = useMinerStore(s => s.setHeaterStatus);
  const pushHistory = useMinerStore(s => s.pushHistory);
  const pushLog = useMinerStore(s => s.pushLog);
  const addToast = useMinerStore(s => s.addToast);
  const markWsFrame = useMinerStore(s => s.markWsFrame);
  const markRestPoll = useMinerStore(s => s.markRestPoll);
  const refreshTransportState = useMinerStore(s => s.refreshTransportState);
  return {
    setWsConnected,
    setStatus,
    setStats,
    setSystemInfo,
    setAutotunerStatus,
    setHeaterStatus,
    pushHistory,
    pushLog,
    addToast,
    markWsFrame,
    markRestPoll,
    refreshTransportState,
  };
}

export default useMinerStore;
