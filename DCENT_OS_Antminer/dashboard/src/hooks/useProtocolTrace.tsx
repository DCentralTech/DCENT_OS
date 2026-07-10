import React, { createContext, useContext, useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../api/client';
import type {
  RecentShareEvent,
  Sv2MessageRecord,
  Sv2MessagesResponse,
  Sv2StatusResponse,
  WsMessage,
  WsMiningSyncMessage,
} from '../api/types';
import { wsManager } from '../api/websocket';
import { useMinerStore } from '../store/miner';
import { useFlightRecorder } from './useFlightRecorder';
import { getLiveWallWatts } from '../utils/power';

type TraceLane = 'pool' | 'protocol' | 'job' | 'dispatch' | 'nonce' | 'share' | 'system';
type TraceSeverity = 'info' | 'success' | 'warning' | 'danger';

interface ProtocolTraceEvent {
  id: number;
  timestamp: number;
  lane: TraceLane;
  severity: TraceSeverity;
  title: string;
  detail: string;
  tags: string[];
}

interface ProtocolTraceSnapshot {
  poolStatus: string;
  protocolVersion: string;
  hashrateGhs: number;
  wallWatts: number | null;
  activeChains: number;
  currentJobId: string | null;
  lastJobAt: number | null;
  lastDispatchAt: number | null;
  lastNonceAt: number | null;
  lastShareAt: number | null;
  lastShareResult: 'accepted' | 'rejected' | 'lucky' | null;
  latestDispatchCount: number;
  latestNonceCount: number;
  acceptedCount: number;
  rejectedCount: number;
  luckyCount: number;
  sv2Connected: boolean;
  autoFallbackActive: boolean;
  lastProtocolMessage: string | null;
}

interface ProtocolTraceContextValue {
  events: ProtocolTraceEvent[];
  snapshot: ProtocolTraceSnapshot;
  sv2Status: Sv2StatusResponse | null;
  sv2Messages: Sv2MessageRecord[];
  clearTimeline: () => void;
  exportTimeline: () => void;
}

const MAX_TRACE_EVENTS = 240;
const MAX_SV2_MESSAGES = 120;
const SV2_POLL_INTERVAL_MS = 4000;
const DISPATCH_TIMELINE_INTERVAL_MS = 1500;
const NONCE_TIMELINE_INTERVAL_MS = 1250;

const defaultSnapshot: ProtocolTraceSnapshot = {
  poolStatus: 'unknown',
  protocolVersion: 'sv1',
  hashrateGhs: 0,
  wallWatts: null,
  activeChains: 0,
  currentJobId: null,
  lastJobAt: null,
  lastDispatchAt: null,
  lastNonceAt: null,
  lastShareAt: null,
  lastShareResult: null,
  latestDispatchCount: 0,
  latestNonceCount: 0,
  acceptedCount: 0,
  rejectedCount: 0,
  luckyCount: 0,
  sv2Connected: false,
  autoFallbackActive: false,
  lastProtocolMessage: null,
};

const ProtocolTraceContext = createContext<ProtocolTraceContextValue>({
  events: [],
  snapshot: defaultSnapshot,
  sv2Status: null,
  sv2Messages: [],
  clearTimeline: () => {},
  exportTimeline: () => {},
});

const SV2_MSG_TYPES: Record<number, { label: string; lane: TraceLane; severity: TraceSeverity }> = {
  0x00: { label: 'Setup', lane: 'protocol', severity: 'info' },
  0x01: { label: 'Setup OK', lane: 'protocol', severity: 'success' },
  0x02: { label: 'Setup Error', lane: 'protocol', severity: 'danger' },
  0x10: { label: 'Open Channel', lane: 'protocol', severity: 'info' },
  0x11: { label: 'Channel Opened', lane: 'protocol', severity: 'success' },
  0x12: { label: 'Open Channel Error', lane: 'protocol', severity: 'danger' },
  0x1c: { label: 'Submit Share', lane: 'share', severity: 'info' },
  0x1d: { label: 'Share Accepted', lane: 'share', severity: 'success' },
  0x1e: { label: 'New Job', lane: 'job', severity: 'info' },
  0x20: { label: 'New Block', lane: 'job', severity: 'warning' },
  0x21: { label: 'Difficulty Change', lane: 'protocol', severity: 'info' },
};

function pushTraceEvent(
  setEvents: React.Dispatch<React.SetStateAction<ProtocolTraceEvent[]>>,
  nextIdRef: React.MutableRefObject<number>,
  lane: TraceLane,
  severity: TraceSeverity,
  title: string,
  detail: string,
  tags: string[] = [],
  timestamp = Date.now(),
) {
  const entry: ProtocolTraceEvent = {
    id: ++nextIdRef.current,
    timestamp,
    lane,
    severity,
    title,
    detail,
    tags,
  };

  setEvents(prev => {
    const next = [...prev, entry];
    return next.length > MAX_TRACE_EVENTS ? next.slice(-MAX_TRACE_EVENTS) : next;
  });
}

function decodeSv2Message(record: Sv2MessageRecord) {
  const known = SV2_MSG_TYPES[record.msg_type];
  const label = known?.label ?? record.msg_name ?? `0x${record.msg_type.toString(16).padStart(2, '0').toUpperCase()}`;
  return {
    title: `${record.direction === 'sent' ? 'Sent' : 'Recv'} ${label}`,
    lane: known?.lane ?? 'protocol',
    severity: known?.severity ?? 'info',
    detail: `${record.payload_size} B · ${record.msg_name || 'wire frame'}`,
  };
}

function formatMiningSyncEvent(message: WsMiningSyncMessage) {
  switch (message.event) {
    case 'job_received':
      return { lane: 'job' as const, severity: 'info' as const, title: 'Pool Job Received', detail: `Job ${message.job_id ?? 'unknown'} arrived from pool` };
    case 'clean_job':
      return { lane: 'job' as const, severity: 'warning' as const, title: 'Clean Job / New Block', detail: `Previous work invalidated for job ${message.job_id ?? 'unknown'}` };
    case 'dispatch_burst':
      return { lane: 'dispatch' as const, severity: 'info' as const, title: 'Dispatch Burst', detail: `${message.count ?? 0} work items pushed toward active chains` };
    case 'nonce_burst':
      return { lane: 'nonce' as const, severity: 'success' as const, title: 'Nonce Burst', detail: `${message.count ?? 0} nonce responses surfaced from hardware` };
    case 'share_accepted':
      return { lane: 'share' as const, severity: 'success' as const, title: 'Share Accepted', detail: `Target ${message.target_difficulty?.toFixed(0) ?? '?'} achieved ${message.difficulty?.toFixed(0) ?? 'unknown'} on job ${message.job_id ?? 'unknown'}` };
    case 'share_rejected':
      return { lane: 'share' as const, severity: 'danger' as const, title: 'Share Rejected', detail: message.error_msg || 'Pool rejected submitted share' };
    case 'lucky_share':
      return { lane: 'share' as const, severity: 'warning' as const, title: 'Lucky Share', detail: `High-value diff ${message.difficulty?.toFixed(0) ?? '?'} on job ${message.job_id ?? 'unknown'}` };
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

export function ProtocolTraceProvider({ children }: { children: React.ReactNode }) {
  const status = useMinerStore(s => s.status);
  const stats = useMinerStore(s => s.stats);
  const { recordAction } = useFlightRecorder();
  const [events, setEvents] = useState<ProtocolTraceEvent[]>([]);
  const [snapshot, setSnapshot] = useState<ProtocolTraceSnapshot>(defaultSnapshot);
  const [sv2Status, setSv2Status] = useState<Sv2StatusResponse | null>(null);
  const [sv2Messages, setSv2Messages] = useState<Sv2MessageRecord[]>([]);

  const nextIdRef = useRef(0);
  const seenSv2Ref = useRef(new Set<string>());
  const seenShareHistoryRef = useRef(new Set<string>());
  const lastDispatchTimelineRef = useRef(0);
  const lastNonceTimelineRef = useRef(0);
  const lastPoolStatusRef = useRef<string | null>(null);
  const ignoreBeforeRef = useRef(0);

  useEffect(() => {
    const power = stats?.power ?? status?.power;
    const liveWallWatts = getLiveWallWatts(power);
    setSnapshot(prev => ({
      ...prev,
      poolStatus: status?.pool?.status ?? prev.poolStatus,
      protocolVersion: status?.pool?.protocol ?? prev.protocolVersion,
      hashrateGhs: status?.hashrate_ghs ?? stats?.hashrate_ghs ?? prev.hashrateGhs,
      wallWatts: liveWallWatts > 0 ? liveWallWatts : null,
      activeChains: Array.isArray(status?.chains)
        ? status.chains.filter(chain => chain.chips > 0).length
        : prev.activeChains,
      autoFallbackActive: status?.pool?.auto_fallback_active ?? prev.autoFallbackActive,
      acceptedCount: status?.accepted ?? prev.acceptedCount,
      rejectedCount: status?.rejected ?? prev.rejectedCount,
    }));

    const nextPoolStatus = status?.pool?.status ?? null;
    if (nextPoolStatus && lastPoolStatusRef.current && nextPoolStatus !== lastPoolStatusRef.current) {
      pushTraceEvent(
        setEvents,
        nextIdRef,
        'pool',
        nextPoolStatus.toLowerCase().includes('disconnect') ? 'warning' : 'info',
        'Pool State Changed',
        `${lastPoolStatusRef.current} -> ${nextPoolStatus}`,
        [status?.pool?.protocol ?? 'sv1'],
      );
    }
    if (nextPoolStatus) {
      lastPoolStatusRef.current = nextPoolStatus;
    }
  }, [status, stats]);

  useEffect(() => {
    return wsManager.subscribe((message: WsMessage) => {
      if (message.type === 'mining_sync') {
        const formatted = formatMiningSyncEvent(message);
        const now = message.timestamp_ms || Date.now();
        if (now < ignoreBeforeRef.current) {
          return;
        }
        setSnapshot(prev => {
          switch (message.event) {
            case 'job_received':
            case 'clean_job':
              return {
                ...prev,
                currentJobId: message.job_id ?? prev.currentJobId,
                lastJobAt: now,
              };
            case 'dispatch_burst':
              return { ...prev, lastDispatchAt: now, latestDispatchCount: message.count ?? prev.latestDispatchCount };
            case 'nonce_burst':
              return { ...prev, lastNonceAt: now, latestNonceCount: message.count ?? prev.latestNonceCount };
            case 'share_accepted':
              return { ...prev, lastShareAt: now, lastShareResult: 'accepted', acceptedCount: prev.acceptedCount + 1 };
            case 'share_rejected':
              return { ...prev, lastShareAt: now, lastShareResult: 'rejected', rejectedCount: prev.rejectedCount + 1 };
            case 'lucky_share':
              return { ...prev, lastShareAt: now, lastShareResult: 'lucky', luckyCount: prev.luckyCount + 1, acceptedCount: prev.acceptedCount + 1 };
          }
        });

        if (message.event === 'dispatch_burst') {
          if (now - lastDispatchTimelineRef.current >= DISPATCH_TIMELINE_INTERVAL_MS) {
            lastDispatchTimelineRef.current = now;
            pushTraceEvent(setEvents, nextIdRef, formatted.lane, formatted.severity, formatted.title, formatted.detail, ['dispatch'], now);
          }
          return;
        }

        if (message.event === 'nonce_burst') {
          if (now - lastNonceTimelineRef.current >= NONCE_TIMELINE_INTERVAL_MS) {
            lastNonceTimelineRef.current = now;
            pushTraceEvent(setEvents, nextIdRef, formatted.lane, formatted.severity, formatted.title, formatted.detail, ['nonce'], now);
          }
          return;
        }

        pushTraceEvent(
          setEvents,
          nextIdRef,
          formatted.lane,
          formatted.severity,
          formatted.title,
          formatted.detail,
          [message.job_id ?? '', message.chain_id != null ? `chain-${message.chain_id}` : ''].filter(Boolean),
          now,
        );
        return;
      }

      if (message.type === 'log' && (message.level === 'warn' || message.level === 'error')) {
        if (message.timestamp < ignoreBeforeRef.current) {
          return;
        }
        pushTraceEvent(
          setEvents,
          nextIdRef,
          'system',
          message.level === 'error' ? 'danger' : 'warning',
          message.level === 'error' ? 'System Error' : 'System Warning',
          message.message,
          [message.source],
          message.timestamp,
        );
      }
    });
  }, []);

  useEffect(() => {
    let cancelled = false;

    const seedShareHistory = async () => {
      try {
        const response = await api.getShareHistory();
        if (cancelled) {
          return;
        }

        const sorted = [...response.events].sort((a, b) => a.timestamp_ms - b.timestamp_ms);
        for (const event of sorted) {
          const key = `${event.timestamp_ms}-${event.result}-${event.job_id}-${event.nonce ?? ''}`;
          if (seenShareHistoryRef.current.has(key)) {
            continue;
          }
          seenShareHistoryRef.current.add(key);
          pushTraceEvent(
            setEvents,
            nextIdRef,
            'share',
            event.result === 'rejected' ? 'danger' : 'success',
            event.result === 'rejected' ? 'Historical Share Rejected' : 'Historical Share Accepted',
            event.result === 'rejected'
              ? event.error_msg || `Job ${event.job_id}`
              : `Job ${event.job_id} target ${event.target_difficulty?.toFixed(0) ?? '?'} achieved ${event.difficulty?.toFixed(0) ?? 'unknown'}`,
            ['history'],
            event.timestamp_ms,
          );
        }
      } catch {
        // Share history is optional; timeline still works from live WS.
      }
    };

    void seedShareHistory();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;

    const pollSv2 = async () => {
      try {
        const [statusResponse, messagesResponse] = await Promise.all([
          api.getSv2Status(),
          api.getSv2Messages(),
        ]);
        if (cancelled) {
          return;
        }

        setSv2Status(statusResponse);
        setSnapshot(prev => ({
          ...prev,
          sv2Connected: statusResponse.connected,
          protocolVersion: statusResponse.protocol_version ?? prev.protocolVersion,
        }));

        const messages = (messagesResponse.messages ?? []).slice(-MAX_SV2_MESSAGES);
        setSv2Messages(messages);

        for (const record of messages) {
          const key = `${record.timestamp_ms}-${record.direction}-${record.msg_type}-${record.payload_size}`;
          if (seenSv2Ref.current.has(key)) {
            continue;
          }
          if (record.timestamp_ms < ignoreBeforeRef.current) {
            continue;
          }
          seenSv2Ref.current.add(key);
          const decoded = decodeSv2Message(record);
          pushTraceEvent(
            setEvents,
            nextIdRef,
            decoded.lane,
            decoded.severity,
            decoded.title,
            decoded.detail,
            [record.msg_name || `0x${record.msg_type.toString(16)}`],
            record.timestamp_ms,
          );
          setSnapshot(prev => ({
            ...prev,
            lastProtocolMessage: decoded.title,
          }));
        }
      } catch {
        if (!cancelled) {
          setSv2Status(null);
          setSnapshot(prev => ({
            ...prev,
            sv2Connected: false,
          }));
        }
      }
    };

    void pollSv2();
    const intervalId = window.setInterval(() => {
      void pollSv2();
    }, SV2_POLL_INTERVAL_MS);

    return () => {
      cancelled = true;
      window.clearInterval(intervalId);
    };
  }, []);

  const clearTimeline = () => {
    ignoreBeforeRef.current = Date.now();
    setEvents([]);
    seenSv2Ref.current.clear();
    seenShareHistoryRef.current.clear();
    lastDispatchTimelineRef.current = 0;
    lastNonceTimelineRef.current = 0;
    recordAction('protocol_timeline_cleared');
  };

  const exportTimeline = () => {
    downloadJson(`dcentos-protocol-trace-${new Date().toISOString().replace(/[:.]/g, '-')}.json`, {
      exportedAt: new Date().toISOString(),
      snapshot,
      sv2Status,
      sv2Messages,
      events,
    });
    recordAction('protocol_timeline_exported', { events: events.length, sv2Messages: sv2Messages.length });
  };

  const value = useMemo<ProtocolTraceContextValue>(() => ({
    events,
    snapshot,
    sv2Status,
    sv2Messages,
    clearTimeline,
    exportTimeline,
  }), [events, snapshot, sv2Messages, sv2Status]);

  return React.createElement(ProtocolTraceContext.Provider, { value }, children);
}

export function useProtocolTrace() {
  return useContext(ProtocolTraceContext);
}
