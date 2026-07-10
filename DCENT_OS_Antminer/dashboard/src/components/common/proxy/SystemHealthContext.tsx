import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { api } from '../../../api/client';
import type { SystemHealthResponse } from '../../../api/types';

export type HonestModeState =
  | 'unknown'
  | 'native'
  | 'proxy_alive'
  | 'proxy_degraded'
  | 'hardware_blocked'
  /**
   *  HIGH-1 (2026-05-24) — `a lab unit`-class XIL S19j Pro mining via the
   *  bosminer-handoff recipe. Both `bosminer.alive === true` AND
   * `dcentrald.is_mining === true` simultaneously (DCENT_OS is dispatching
   * work, bosminer was the cold-boot bring-up that pre-engaged the chip
   * rail + dsPIC + Loki spoof). The mode is auto-detected on
   * `platform === "zynq-bm3-am2"` + `board_target.endsWith("xil")`.
   * Visual treatment: orange-green hybrid (active mining, but with the
   * AC-cycle dependency caveat the operator must understand).
   */
  | 'handoff_mining';

interface SystemHealthContextValue {
  health: SystemHealthResponse | null;
  endpointAvailable: boolean | null;
  lastError: string | null;
  state: HonestModeState;
  isProxyMode: boolean;
  refresh: () => Promise<void>;
}

const SystemHealthContext = createContext<SystemHealthContextValue>({
  health: null,
  endpointAvailable: null,
  lastError: null,
  state: 'unknown',
  isProxyMode: false,
  refresh: async () => {},
});

export function SystemHealthProvider({ children }: { children: ReactNode }) {
  const [health, setHealth] = useState<SystemHealthResponse | null>(null);
  const [endpointAvailable, setEndpointAvailable] = useState<boolean | null>(null);
  const [lastError, setLastError] = useState<string | null>(null);
  const endpointMissingRef = useRef(false);

  const refresh = useCallback(async () => {
    if (endpointMissingRef.current) {
      return;
    }

    try {
      const next = await api.getSystemHealth();
      if (next == null) {
        endpointMissingRef.current = true;
        setEndpointAvailable(false);
        setHealth(null);
        setLastError(null);
        return;
      }

      setEndpointAvailable(true);
      setHealth(next);
      setLastError(null);
    } catch (err) {
      setLastError(err instanceof Error ? err.message : 'Unable to fetch system health');
    }
  }, []);

  useEffect(() => {
    let cancelled = false;

    const guardedRefresh = async () => {
      if (cancelled) {
        return;
      }
      await refresh();
    };

    guardedRefresh();
    const timer = setInterval(guardedRefresh, 10000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [refresh]);

  const state = getHonestModeState(health);
  const isProxyMode = isProxyRuntime(health);
  const value = useMemo<SystemHealthContextValue>(() => ({
    health,
    endpointAvailable,
    lastError,
    state,
    isProxyMode,
    refresh,
  }), [endpointAvailable, health, isProxyMode, lastError, refresh, state]);

  return (
    <SystemHealthContext.Provider value={value}>
      {children}
    </SystemHealthContext.Provider>
  );
}

export function useSystemHealth() {
  return useContext(SystemHealthContext);
}

export function isProxyRuntime(health: SystemHealthResponse | null | undefined): boolean {
  const mode = String(health?.mode ?? '').toLowerCase();
  return mode === 'proxy' || mode === 'hybrid';
}

export function normalizeEpochMs(value: number | null | undefined): number | null {
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) {
    return null;
  }
  return value < 10_000_000_000 ? value * 1000 : value;
}

export function getLastSeenAgeMs(health: SystemHealthResponse | null | undefined): number | null {
  const lastSeenMs = normalizeEpochMs(health?.bosminer?.last_seen_ms);
  if (lastSeenMs == null) {
    return null;
  }
  return Math.max(0, Date.now() - lastSeenMs);
}

/**
 *  HIGH-1 (2026-05-24): true when the snapshot describes a
 * `a lab unit`-class XIL S19j Pro unit running the  bosminer-handoff
 * mining recipe — `bosminer.alive === true` (cold-boot bring-up that
 * pre-engaged the chip rail + dsPIC + Loki spoof) AND
 * `daemon.is_mining === true` (DCENT_OS is dispatching work).
 *
 * Hardware fingerprint MUST match: `platform === "zynq-bm3-am2"` and
 * `board_target` ends with `xil`. Other AM2 XIL units (`a lab unit`,
 * `a lab unit`) do NOT match — only `a lab unit` is documented to run the recipe.
 */
export function isHandoffMiningRuntime(
  health: SystemHealthResponse | null | undefined,
): boolean {
  if (!health) return false;
  const bosminerAlive = health.bosminer?.alive === true;
  const isMining = health.daemon?.is_mining === true;
  if (!bosminerAlive || !isMining) return false;
  // Trust the daemon-side `is_xil_25_class` flag if present (the REST
  // handler computes it from the fingerprint files); otherwise fall back
  // to the same suffix-match the runtime guard uses.
  const fp = health.fingerprint;
  if (fp?.is_xil_25_class === true) return true;
  const platform = String(fp?.platform ?? '').trim();
  const boardTarget = String(fp?.board_target ?? '').trim();
  return platform === 'zynq-bm3-am2' && boardTarget.endsWith('xil');
}

export function getHonestModeState(health: SystemHealthResponse | null | undefined): HonestModeState {
  if (!health) {
    return 'unknown';
  }

  //  HIGH-1:  bosminer-handoff path on `a lab unit`-class XIL.
  // Check BEFORE the native/proxy fork — bosminer.alive + dcentrald
  // dispatching work simultaneously would otherwise display as
  // "proxy mode, not us" even though we ARE the chain driver.
  if (isHandoffMiningRuntime(health)) {
    return 'handoff_mining';
  }

  if (!isProxyRuntime(health)) {
    return 'native';
  }

  const bosminerAlive = health.bosminer?.alive === true;
  const blockers = health.bosminer?.blockers ?? [];
  const railVerdict = String(health.rail?.verdict ?? '').toUpperCase();
  const ageMs = getLastSeenAgeMs(health);

  const railBlocked = railVerdict === 'DEAD' || railVerdict === 'PARTIAL';
  const railPendingAfterDrop = railVerdict === 'PENDING' && !bosminerAlive;
  const controllerRejected = blockers.some(blocker => blocker === 'fw_86_rejection');
  const deadLongEnough = !bosminerAlive && (ageMs == null || ageMs > 5 * 60 * 1000);

  if (railBlocked || railPendingAfterDrop || controllerRejected || deadLongEnough) {
    return 'hardware_blocked';
  }

  if (bosminerAlive) {
    return 'proxy_alive';
  }

  return 'proxy_degraded';
}
