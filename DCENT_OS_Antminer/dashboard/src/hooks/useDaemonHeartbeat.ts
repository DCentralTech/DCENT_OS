// useDaemonHeartbeat — polls server.py's `/api/dashboard/health` shim every
// 3 seconds and exposes daemon liveness so the dashboard can degrade
// gracefully when dcentrald is dead/restarting/stuck.
//
// server.py serves this endpoint locally even when dcentrald is offline,
// so it gives us a true ground truth (pid alive, last log lines, uptime)
// without needing to talk to dcentrald at all.

import { useEffect, useRef, useState } from 'react';
import { DAEMON_DISCONNECTED_EVENT, DAEMON_RECONNECTED_EVENT } from '../api/client';

export type DaemonState = 'alive' | 'starting' | 'dead' | 'unknown';

export interface DaemonHealth {
  state: DaemonState;
  pidAlive: boolean;
  pid: number | null;
  uptimeSec: number | null;
  lastSeenSec: number | null;     // seconds since we last had a successful API contact
  lastLogLines: string[];
  lastError: string | null;
  lastProbeTs: number | null;     // wall-clock ms of last health probe
  lastApiSuccessTs: number | null; // wall-clock ms of last successful dcentrald API contact
  serverPyAlive: boolean;          // server.py itself responding
}

interface DashboardHealthResponse {
  pid?: number | null;
  alive?: boolean;
  uptime_s?: number | null;
  last_log_lines?: string[];
  last_health_probe_ts?: number | null;
}

const POLL_INTERVAL_MS = 3000;
const STARTING_GRACE_MS = 30000;   // dcentrald init can take ~25s
const STALE_THRESHOLD_SEC = 10;    // healthy = saw a heartbeat in last 10s

export function useDaemonHeartbeat(): DaemonHealth {
  const [health, setHealth] = useState<DaemonHealth>({
    state: 'unknown',
    pidAlive: false,
    pid: null,
    uptimeSec: null,
    lastSeenSec: null,
    lastLogLines: [],
    lastError: null,
    lastProbeTs: null,
    lastApiSuccessTs: null,
    serverPyAlive: false,
  });

  // Track when we last saw any successful api call (separate from heartbeat
  // probe — successful API calls also count as alive evidence).
  const lastApiSuccessRef = useRef<number>(0);
  const startTimeRef = useRef<number>(Date.now());

  useEffect(() => {
    // Listen for global daemon-disconnect events from api/client.ts
    const onDisconnected = () => {
      // We don't immediately flip to 'dead' — let the heartbeat poll confirm,
      // but record the event so the banner UI can react fast.
      const now = Date.now();
      const lastSeenSec = lastApiSuccessRef.current
        ? Math.max(0, Math.round((now - lastApiSuccessRef.current) / 1000))
        : null;
      setHealth(prev => ({ ...prev, lastError: 'API call failed', lastSeenSec }));
    };
    const onReconnected = () => {
      const now = Date.now();
      lastApiSuccessRef.current = now;
      setHealth(prev => ({ ...prev, lastError: null, lastSeenSec: 0, lastApiSuccessTs: now }));
    };

    window.addEventListener(DAEMON_DISCONNECTED_EVENT, onDisconnected as EventListener);
    window.addEventListener(DAEMON_RECONNECTED_EVENT, onReconnected as EventListener);

    return () => {
      window.removeEventListener(DAEMON_DISCONNECTED_EVENT, onDisconnected as EventListener);
      window.removeEventListener(DAEMON_RECONNECTED_EVENT, onReconnected as EventListener);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let consecutiveFailures = 0;

    const probe = async () => {
      try {
        const res = await fetch('/api/dashboard/health', {
          signal: AbortSignal.timeout(2500),
        });
        if (!res.ok) {
          throw new Error(`HTTP ${res.status}`);
        }
        const json = (await res.json()) as DashboardHealthResponse;
        if (cancelled) return;

        consecutiveFailures = 0;
        const now = Date.now();
        const pidAlive = !!json.alive;
        // Decide visible state.
        // - pid alive AND we've had a recent API success → alive
        // - pid alive but no API success yet → starting (within grace window),
        //   else dead
        // - pid dead → dead
        let state: DaemonState;
        const sinceStartMs = now - startTimeRef.current;
        const apiAgeMs = lastApiSuccessRef.current
          ? now - lastApiSuccessRef.current
          : Infinity;

        if (!pidAlive) {
          state = 'dead';
        } else if (apiAgeMs <= STALE_THRESHOLD_SEC * 1000) {
          state = 'alive';
        } else if (sinceStartMs <= STARTING_GRACE_MS) {
          state = 'starting';
        } else {
          state = 'dead';
        }

        const lastSeenSec = lastApiSuccessRef.current
          ? Math.max(0, Math.round(apiAgeMs / 1000))
          : null;

        setHealth({
          state,
          pidAlive,
          pid: json.pid ?? null,
          uptimeSec: json.uptime_s ?? null,
          lastSeenSec,
          lastLogLines: Array.isArray(json.last_log_lines) ? json.last_log_lines : [],
          lastError: null,
          lastProbeTs: now,
          lastApiSuccessTs: lastApiSuccessRef.current || null,
          serverPyAlive: true,
        });
      } catch (e) {
        if (cancelled) return;
        consecutiveFailures += 1;
        const now = Date.now();
        const lastSeenSec = lastApiSuccessRef.current
          ? Math.max(0, Math.round((now - lastApiSuccessRef.current) / 1000))
          : null;
        // server.py itself is down — really bad, but show something useful.
        setHealth(prev => ({
          ...prev,
          state: consecutiveFailures >= 2 ? 'dead' : prev.state,
          lastError: e instanceof Error ? e.message : 'unknown error',
          lastSeenSec,
          lastProbeTs: now,
          lastApiSuccessTs: lastApiSuccessRef.current || prev.lastApiSuccessTs,
          serverPyAlive: false,
        }));
      }
    };

    // Immediate first probe + interval
    probe();
    const timer = setInterval(probe, POLL_INTERVAL_MS);

    // Track API successes via the same event that api/client.ts dispatches
    const onReconnected = () => {
      const now = Date.now();
      lastApiSuccessRef.current = now;
      setHealth(prev => ({ ...prev, lastSeenSec: 0, lastApiSuccessTs: now }));
    };
    window.addEventListener(DAEMON_RECONNECTED_EVENT, onReconnected as EventListener);

    return () => {
      cancelled = true;
      clearInterval(timer);
      window.removeEventListener(DAEMON_RECONNECTED_EVENT, onReconnected as EventListener);
    };
  }, []);

  return health;
}
