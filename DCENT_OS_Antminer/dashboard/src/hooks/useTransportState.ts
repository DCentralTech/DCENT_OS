import { useEffect, useMemo, useState } from 'react';
import {
  deriveTransportKind,
  useMinerStore,
  type TransportKind,
} from '../store/miner';

export interface TransportViewState {
  transport: TransportKind;
  label: 'LIVE' | 'POLLING' | 'STALE';
  tone: 'success' | 'warning' | 'danger';
  title: string;
}

function formatAge(now: number, at: number): string {
  if (!at) return 'never';
  const ageS = Math.max(0, Math.round((now - at) / 1000));
  return `${ageS}s ago`;
}

export function describeTransportState(
  transport: TransportKind,
  now: number,
  lastWsFrameAt: number,
  lastRestPollAt: number,
): TransportViewState {
  if (transport === 'ws-live') {
    return {
      transport,
      label: 'LIVE',
      tone: 'success',
      title: `WebSocket frame received ${formatAge(now, lastWsFrameAt)}.`,
    };
  }
  if (transport === 'rest-polling') {
    return {
      transport,
      label: 'POLLING',
      tone: 'warning',
      title: `REST polling fallback last succeeded ${formatAge(now, lastRestPollAt)}.`,
    };
  }
  return {
    transport,
    label: 'STALE',
    tone: 'danger',
    title: 'No recent WebSocket frame or REST polling sample.',
  };
}

export function useTransportState(): TransportViewState {
  const wsConnected = useMinerStore(s => s.wsConnected);
  const lastWsFrameAt = useMinerStore(s => s.lastWsFrameAt);
  const lastRestPollAt = useMinerStore(s => s.lastRestPollAt);
  const storeTransport = useMinerStore(s => s.transport);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, []);

  return useMemo(() => {
    const transport = deriveTransportKind(now, wsConnected, lastWsFrameAt, lastRestPollAt);
    return describeTransportState(
      transport === storeTransport ? storeTransport : transport,
      now,
      lastWsFrameAt,
      lastRestPollAt,
    );
  }, [lastRestPollAt, lastWsFrameAt, now, storeTransport, wsConnected]);
}
