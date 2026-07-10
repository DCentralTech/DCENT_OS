import React, { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from 'react';
import { wsManager } from '../api/websocket';
import type { WsMessage } from '../api/types';
import { useMinerStore } from '../store/miner';
import { getLiveWallWatts, getPowerTelemetryLabel } from '../utils/power';
import { wattsToBtu } from '../utils/thermal';

type LivePowerTelemetry = Parameters<typeof getLiveWallWatts>[0];
type HeaterWsMessage = Extract<WsMessage, { type: 'heater_status' }>;

export type FlightRecorderEntrySource = 'ws' | 'snapshot' | 'action' | 'nav' | 'marker';

export interface FlightRecorderEntry {
  id: number;
  timestamp: number;
  source: FlightRecorderEntrySource;
  event: string;
  detail: Record<string, unknown>;
}

interface FlightRecorderContextValue {
  frozen: boolean;
  startedAt: number;
  lastUpdatedAt: number;
  totalCaptured: number;
  entries: FlightRecorderEntry[];
  freeze: () => void;
  resume: () => void;
  clear: () => void;
  exportBundle: () => void;
  recordAction: (event: string, detail?: Record<string, unknown>) => void;
  addMarker: (label: string) => void;
}

const MAX_ENTRIES = 1600;
const WINDOW_MS = 5 * 60 * 1000;
const SNAPSHOT_INTERVAL_MS = 15000;

const FlightRecorderContext = createContext<FlightRecorderContextValue>({
  frozen: false,
  startedAt: Date.now(),
  lastUpdatedAt: Date.now(),
  totalCaptured: 0,
  entries: [],
  freeze: () => {},
  resume: () => {},
  clear: () => {},
  exportBundle: () => {},
  recordAction: () => {},
  addMarker: () => {},
});

function pruneEntries(entries: FlightRecorderEntry[], now: number) {
  const recent = entries.filter(entry => now - entry.timestamp <= WINDOW_MS);
  return recent.length > MAX_ENTRIES ? recent.slice(-MAX_ENTRIES) : recent;
}

function livePowerRecorderDetail(power: LivePowerTelemetry) {
  const wallWatts = getLiveWallWatts(power);
  const data = power && typeof power === 'object' ? power as Record<string, unknown> : {};
  return {
    wallWatts: wallWatts > 0 ? wallWatts : null,
    wallPowerLive: wallWatts > 0,
    wallPowerSource: typeof data.source === 'string' ? data.source : null,
    wallPowerSourceDetail: typeof data.source_detail === 'string'
      ? data.source_detail
      : typeof data.power_source_detail === 'string'
        ? data.power_source_detail
        : null,
    wallPowerNote: getPowerTelemetryLabel(power) ?? 'Power telemetry unavailable',
  };
}

function heaterWsHasPowerProvenance(message: HeaterWsMessage): boolean {
  return (
    message.power_source !== undefined ||
    message.power_source_detail !== undefined ||
    message.live_power_available !== undefined ||
    message.power_modeled !== undefined ||
    message.power_note !== undefined ||
    message.power_calibrated !== undefined ||
    message.power_calibration_multiplier !== undefined
  );
}

function heaterPowerRecorderDetail(message: HeaterWsMessage) {
  const hasProvenance = heaterWsHasPowerProvenance(message);
  const power = {
    wall_watts: message.wall_watts ?? null,
    source: hasProvenance ? message.power_source : 'static_model_fallback',
    source_detail: hasProvenance ? message.power_source_detail : 'static_power_fallback_from_miner_state',
    live_power_available: hasProvenance ? (message.live_power_available ?? false) : false,
    modeled: hasProvenance ? (message.power_modeled ?? true) : true,
    note: hasProvenance
      ? message.power_note
      : 'Heater WebSocket power lacks live provenance; REST /api/home/status provides live wall-power labels.',
    calibrated: hasProvenance ? message.power_calibrated : false,
    calibration_multiplier: hasProvenance ? (message.power_calibration_multiplier ?? null) : null,
  };
  const liveWallWatts = getLiveWallWatts(power);
  const liveBtuH = liveWallWatts > 0 ? wattsToBtu(liveWallWatts) : null;
  return {
    powerWatts: message.power_watts,
    reportedBtuH: message.btu_h,
    btuH: liveBtuH,
    btuLive: liveBtuH !== null,
    btuNote: liveBtuH !== null
      ? 'Computed from live wall power'
      : 'Reported heater BTU is not treated as live without wall-power provenance',
    ...livePowerRecorderDetail(power),
  };
}

function summarizeWsMessage(message: WsMessage): { event: string; detail: Record<string, unknown> } {
  switch (message.type) {
    case 'stats': {
      const power = {
        watts: message.power_watts,
        wall_watts: message.wall_watts,
        source: message.power_source,
        source_detail: message.power_source_detail,
        live_power_available: message.live_power_available,
        modeled: message.power_modeled,
        note: message.power_note,
        calibrated: message.power_calibrated,
        calibration_multiplier: message.power_calibration_multiplier,
      };
      return {
        event: 'stats',
        detail: {
          hashrateGhs: message.hashrate_ghs,
          accepted: message.accepted,
          rejected: message.rejected,
          poolStatus: message.pool.status,
          ...livePowerRecorderDetail(power),
        },
      };
    }
    case 'log':
      return {
        event: `log:${message.level}`,
        detail: {
          level: message.level,
          source: message.source,
          message: message.message,
        },
      };
    case 'diagnostic_progress':
      return {
        event: 'diagnostic_progress',
        detail: {
          testId: message.test_id,
          phase: message.phase,
          progressPct: message.progress_pct,
          detail: message.detail,
        },
      };
    case 'heater_status':
      return {
        event: 'heater_status',
        detail: {
          ...heaterPowerRecorderDetail(message),
          preset: message.preset,
        },
      };
    case 'autotuner_status':
      return {
        event: 'autotuner_status',
        detail: {
          state: message.payload.state,
          phase: message.payload.phase,
          percentComplete: message.payload.percent_complete,
          message: message.payload.message,
        },
      };
    case 'autotuner_efficiency':
      return {
        event: 'autotuner_efficiency',
        detail: {
          payloadKeys: Object.keys(message.payload),
        },
      };
    case 'autotuner_chip_health':
      return {
        event: 'autotuner_chip_health',
        detail: {
          totalChips: message.payload.total_chips,
          message: message.payload.message,
        },
      };
    case 'mining_sync':
      return {
        event: `mining_sync:${message.event}`,
        detail: {
          event: message.event,
          chainId: message.chain_id ?? null,
          count: message.count ?? null,
          jobId: message.job_id ?? null,
          difficulty: message.difficulty ?? null,
          targetDifficulty: message.target_difficulty ?? null,
          intensity: message.intensity ?? null,
          errorCode: message.error_code ?? null,
          errorMsg: message.error_msg ?? null,
        },
      };
  }
}

function downloadJson(filename: string, payload: unknown) {
  const json = JSON.stringify(payload, null, 2);
  const blob = new Blob([json], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  document.body.removeChild(anchor);
  URL.revokeObjectURL(url);
}

export function FlightRecorderProvider({ children }: { children: React.ReactNode }) {
  const currentPage = useMinerStore(s => s.currentPage);
  const [entries, setEntries] = useState<FlightRecorderEntry[]>([]);
  const [frozen, setFrozen] = useState(false);
  const [startedAt, setStartedAt] = useState(() => Date.now());
  const [lastUpdatedAt, setLastUpdatedAt] = useState(() => Date.now());
  const [totalCaptured, setTotalCaptured] = useState(0);

  const nextIdRef = useRef(0);
  const frozenRef = useRef(frozen);
  const currentPageRef = useRef(currentPage);

  useEffect(() => {
    frozenRef.current = frozen;
  }, [frozen]);

  const pushEntry = useCallback((source: FlightRecorderEntrySource, event: string, detail: Record<string, unknown>) => {
    if (frozenRef.current) {
      return;
    }

    const now = Date.now();
    const entry: FlightRecorderEntry = {
      id: ++nextIdRef.current,
      timestamp: now,
      source,
      event,
      detail,
    };

    setEntries(prev => pruneEntries([...prev, entry], now));
    setLastUpdatedAt(now);
    setTotalCaptured(prev => prev + 1);
  }, []);

  const recordAction = useCallback((event: string, detail: Record<string, unknown> = {}) => {
    pushEntry('action', event, detail);
  }, [pushEntry]);

  const addMarker = useCallback((label: string) => {
    pushEntry('marker', 'marker', { label });
  }, [pushEntry]);

  const clear = useCallback(() => {
    const now = Date.now();
    setEntries([]);
    setStartedAt(now);
    setLastUpdatedAt(now);
    setTotalCaptured(0);
  }, []);

  const freeze = useCallback(() => {
    frozenRef.current = true;
    setFrozen(true);
  }, []);

  const resume = useCallback(() => {
    frozenRef.current = false;
    setFrozen(false);
    setLastUpdatedAt(Date.now());
  }, []);

  const exportBundle = useCallback(() => {
    const state = useMinerStore.getState();
    const bundle = {
      exportedAt: new Date().toISOString(),
      windowSeconds: WINDOW_MS / 1000,
      startedAt: new Date(startedAt).toISOString(),
      frozen,
      totalCaptured,
      navigation: {
        currentPage: state.currentPage,
        mode: state.mode,
      },
      summary: {
        alertsOpen: state.alerts.filter(alert => !alert.dismissed).length,
        logEntries: state.logEntries.length,
        hashrateHistoryPoints: state.hashrateHistory.length,
        tempHistoryPoints: state.tempHistory.length,
        powerHistoryPoints: state.powerHistory.length,
      },
      current: {
        status: state.status,
        systemInfo: state.systemInfo,
        stats: state.stats,
        alerts: state.alerts,
        logs: state.logEntries.slice(-200),
      },
      entries,
    };
    downloadJson(`dcentos-flight-recorder-${new Date().toISOString().replace(/[:.]/g, '-')}.json`, bundle);
  }, [entries, frozen, startedAt, totalCaptured]);

  useEffect(() => {
    return wsManager.subscribe(message => {
      const summary = summarizeWsMessage(message);
      pushEntry('ws', summary.event, summary.detail);
    });
  }, [pushEntry]);

  useEffect(() => {
    if (currentPageRef.current !== currentPage) {
      pushEntry('nav', 'navigate', {
        from: currentPageRef.current,
        to: currentPage,
      });
      currentPageRef.current = currentPage;
    }
  }, [currentPage, pushEntry]);

  useEffect(() => {
    const intervalId = window.setInterval(() => {
      if (frozenRef.current) {
        return;
      }

      const state = useMinerStore.getState();
      const power = state.stats?.power ?? state.status?.power;
      pushEntry('snapshot', 'runtime_snapshot', {
        page: state.currentPage,
        mode: state.mode,
        hashrateGhs: state.status?.hashrate_ghs ?? 0,
        accepted: state.status?.accepted ?? 0,
        rejected: state.status?.rejected ?? 0,
        poolStatus: state.status?.pool?.status ?? 'unknown',
        ...livePowerRecorderDetail(power),
        openAlerts: state.alerts.filter(alert => !alert.dismissed).length,
      });
    }, SNAPSHOT_INTERVAL_MS);

    return () => window.clearInterval(intervalId);
  }, [pushEntry]);

  const value = useMemo<FlightRecorderContextValue>(() => ({
    frozen,
    startedAt,
    lastUpdatedAt,
    totalCaptured,
    entries,
    freeze,
    resume,
    clear,
    exportBundle,
    recordAction,
    addMarker,
  }), [clear, entries, exportBundle, freeze, frozen, lastUpdatedAt, recordAction, resume, startedAt, totalCaptured, addMarker]);

  return React.createElement(FlightRecorderContext.Provider, { value }, children);
}

export function useFlightRecorder() {
  return useContext(FlightRecorderContext);
}
