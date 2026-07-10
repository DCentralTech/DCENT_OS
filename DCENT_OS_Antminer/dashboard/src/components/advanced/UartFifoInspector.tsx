import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../../api/client';
import type { WsMessage, WsMiningSyncMessage } from '../../api/types';
import { wsManager } from '../../api/websocket';
import { formatHex } from '../../utils/format';
import { useFlightRecorder } from '../../hooks/useFlightRecorder';
import { useActiveHardware } from '../../hooks/useActiveHardware';
import { SvgChart, type ChartSeries } from '../common/SvgChart';

type ChainId = 6 | 7 | 8;

interface FifoFlags {
  cmdRxEmpty: boolean;
  cmdRxFull: boolean;
  workRxEmpty: boolean;
  workRxFull: boolean;
  workTxEmpty: boolean;
  workTxFull: boolean;
  irqPending: boolean;
}

interface ChainSnapshot {
  chainId: ChainId;
  timestamp: number;
  ctrl: number;
  buildId: number;
  baud: number;
  stat: number;
  errCnt: number;
  workId: number;
  workTime: number;
  ticketMask: number;
  hashCntLo: number;
  hashCntHi: number;
  hashCount64: number;
  flags: FifoFlags;
}

interface MiningOverlay {
  event: string;
  timestamp: number;
  count: number;
  chainId: number | null;
  detail: string;
}

const CHAINS: ChainId[] = [6, 7, 8];
const POLL_INTERVAL_OPTIONS = [500, 1000, 2000, 5000] as const;
const REG_READ_OFFSET = '0x0000';
const REG_READ_COUNT = 40;
const MAX_HISTORY = 48;
const MAX_OVERLAYS = 36;

function parseU32(values: number[], offset: number) {
  if (!values || offset + 4 > values.length) {
    return 0;
  }
  return (((values[offset] << 24) >>> 0) | (values[offset + 1] << 16) | (values[offset + 2] << 8) | values[offset + 3]) >>> 0;
}

function decodeFlags(stat: number): FifoFlags {
  return {
    cmdRxEmpty: (stat & (1 << 0)) !== 0,
    cmdRxFull: (stat & (1 << 1)) !== 0,
    workRxEmpty: (stat & (1 << 2)) !== 0,
    workRxFull: (stat & (1 << 3)) !== 0,
    workTxEmpty: (stat & (1 << 4)) !== 0,
    workTxFull: (stat & (1 << 5)) !== 0,
    irqPending: (stat & (1 << 6)) !== 0,
  };
}

function parseSnapshot(chainId: ChainId, values: number[]): ChainSnapshot {
  const ctrl = parseU32(values, 0);
  const buildId = parseU32(values, 4);
  const baud = parseU32(values, 8);
  const stat = parseU32(values, 12);
  const errCnt = parseU32(values, 16);
  const workId = parseU32(values, 20);
  const workTime = parseU32(values, 24);
  const ticketMask = parseU32(values, 28);
  const hashCntLo = parseU32(values, 32);
  const hashCntHi = parseU32(values, 36);
  return {
    chainId,
    timestamp: Date.now(),
    ctrl,
    buildId,
    baud,
    stat,
    errCnt,
    workId,
    workTime,
    ticketMask,
    hashCntLo,
    hashCntHi,
    hashCount64: hashCntLo + (hashCntHi * 0x1_0000_0000),
    flags: decodeFlags(stat),
  };
}

function formatTime(timestamp: number) {
  return new Date(timestamp).toLocaleTimeString('en-US', {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

function formatBaud(baudDiv: number) {
  if (baudDiv < 0) {
    return 'n/a';
  }
  const hz = 200_000_000 / (16 * (baudDiv + 1));
  return `${Math.round(hz).toLocaleString()} baud`;
}

function buildOverlay(message: WsMiningSyncMessage): MiningOverlay {
  switch (message.event) {
    case 'job_received':
      return { event: 'job', timestamp: message.timestamp_ms, count: 1, chainId: message.chain_id ?? null, detail: `Job ${message.job_id ?? 'unknown'} arrived` };
    case 'clean_job':
      return { event: 'clean', timestamp: message.timestamp_ms, count: 1, chainId: message.chain_id ?? null, detail: `Clean job for ${message.job_id ?? 'unknown'}` };
    case 'dispatch_burst':
      return { event: 'dispatch', timestamp: message.timestamp_ms, count: message.count ?? 0, chainId: message.chain_id ?? null, detail: `${message.count ?? 0} work items dispatched` };
    case 'nonce_burst':
      return { event: 'nonce', timestamp: message.timestamp_ms, count: message.count ?? 0, chainId: message.chain_id ?? null, detail: `${message.count ?? 0} nonce responses surfaced` };
    case 'share_accepted':
      return { event: 'share+', timestamp: message.timestamp_ms, count: 1, chainId: message.chain_id ?? null, detail: `Accepted diff ${message.difficulty?.toFixed(0) ?? '?'}` };
    case 'lucky_share':
      return { event: 'lucky', timestamp: message.timestamp_ms, count: 1, chainId: message.chain_id ?? null, detail: `Lucky diff ${message.difficulty?.toFixed(0) ?? '?'}` };
    case 'share_rejected':
      return { event: 'share-', timestamp: message.timestamp_ms, count: 1, chainId: message.chain_id ?? null, detail: message.error_msg || 'Rejected share' };
  }
}

function clampHistory<T>(items: T[], limit: number) {
  return items.length > limit ? items.slice(-limit) : items;
}

export function UartFifoInspector() {
  const { activeChain, setActiveChain } = useActiveHardware();
  const { recordAction } = useFlightRecorder();
  const [focusChain, setFocusChain] = useState<number | 'all'>(activeChain);
  const [pollIntervalMs, setPollIntervalMs] = useState<(typeof POLL_INTERVAL_OPTIONS)[number]>(1000);
  const [running, setRunning] = useState(true);
  const [error, setError] = useState('');
  const [snapshotsByChain, setSnapshotsByChain] = useState<Record<ChainId, ChainSnapshot[]>>({ 6: [], 7: [], 8: [] });
  const [overlays, setOverlays] = useState<MiningOverlay[]>([]);
  const pollRef = useRef<number | null>(null);

  useEffect(() => {
    if (focusChain !== 'all') {
      setActiveChain(focusChain);
    }
  }, [focusChain, setActiveChain]);

  const pollRegisters = useCallback(async () => {
    try {
      const responses = await Promise.all(CHAINS.map(chainId => api.readRegisters(chainId, REG_READ_OFFSET, REG_READ_COUNT)));
      const now = Date.now();
      setSnapshotsByChain(prev => {
        const next = { ...prev };
        for (const response of responses) {
          const chainId = response.chain as ChainId;
          const snapshot = parseSnapshot(chainId, response.values);
          next[chainId] = clampHistory([...prev[chainId], { ...snapshot, timestamp: now }], MAX_HISTORY);
        }
        return next;
      });
      setError('');
    } catch (nextError: unknown) {
      setError(nextError instanceof Error ? nextError.message : 'Register polling failed');
    }
  }, []);

  useEffect(() => {
    if (!running) {
      if (pollRef.current) {
        window.clearInterval(pollRef.current);
        pollRef.current = null;
      }
      return;
    }

    void pollRegisters();
    pollRef.current = window.setInterval(() => {
      void pollRegisters();
    }, pollIntervalMs);

    return () => {
      if (pollRef.current) {
        window.clearInterval(pollRef.current);
        pollRef.current = null;
      }
    };
  }, [pollIntervalMs, pollRegisters, running]);

  useEffect(() => {
    return wsManager.subscribe((message: WsMessage) => {
      if (message.type !== 'mining_sync') {
        return;
      }
      const overlay = buildOverlay(message);
      setOverlays(prev => clampHistory([...prev, overlay], MAX_OVERLAYS));
    });
  }, []);

  const latestByChain = useMemo(() => {
    return CHAINS.map(chainId => ({
      chainId,
      latest: snapshotsByChain[chainId][snapshotsByChain[chainId].length - 1] ?? null,
      history: snapshotsByChain[chainId],
    }));
  }, [snapshotsByChain]);

  const visibleChains = focusChain === 'all'
    ? latestByChain
    : latestByChain.filter(entry => entry.chainId === focusChain);

  const overlaySummary = useMemo(() => {
    const summary = {
      dispatch: 0,
      nonce: 0,
      accepted: 0,
      rejected: 0,
      lucky: 0,
    };
    for (const overlay of overlays) {
      if (overlay.event === 'dispatch') summary.dispatch += overlay.count;
      if (overlay.event === 'nonce') summary.nonce += overlay.count;
      if (overlay.event === 'share+') summary.accepted += overlay.count;
      if (overlay.event === 'share-') summary.rejected += overlay.count;
      if (overlay.event === 'lucky') summary.lucky += overlay.count;
    }
    return summary;
  }, [overlays]);

  const exportInspector = () => {
    const payload = {
      exportedAt: new Date().toISOString(),
      focusChain,
      pollIntervalMs,
      overlays,
      snapshotsByChain,
    };
    const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = `dcentos-uart-fifo-inspector-${Date.now()}.json`;
    document.body.appendChild(anchor);
    anchor.click();
    document.body.removeChild(anchor);
    URL.revokeObjectURL(url);
    recordAction('uart_fifo_exported', { focusChain, snapshots: latestByChain.reduce((sum, entry) => sum + entry.history.length, 0) });
  };

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// uart fifo inspector</div>
          <h2 className="hacker-inspector-title">Chain UART / FIFO State</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${running ? '' : 'warning'}`}>{running ? `POLL ${pollIntervalMs}ms` : 'PAUSED'}</span>
          <button
            className="hacker-inspector-help"
            onClick={() => {
              setRunning(value => !value);
              recordAction('uart_fifo_poll_toggled', { running: !running });
            }}
          >
            {running ? '⏸ PAUSE' : '▶ RESUME'}
          </button>
          <button className="hacker-inspector-help" onClick={() => { void pollRegisters(); }}>⟳ NOW</button>
          <button className="hacker-inspector-refresh" onClick={exportInspector}>⤓ EXPORT</button>
        </div>
      </header>

      <div className="hacker-inspector-body">
      <div className="register-inspector adv-card-mb">
        <div className="advanced-inline-actions uf-controls">
          <div>
            <label className="advanced-control-label" htmlFor="uart-fifo-chain">Chain Focus</label>
            <select id="uart-fifo-chain" value={focusChain} onChange={event => setFocusChain(event.target.value === 'all' ? 'all' : Number(event.target.value))}>
              <option value="all">All Chains</option>
              <option value={6}>Chain 6</option>
              <option value={7}>Chain 7</option>
              <option value={8}>Chain 8</option>
            </select>
          </div>
          <div>
            <label className="advanced-control-label" htmlFor="uart-fifo-interval">Poll Interval</label>
            <select id="uart-fifo-interval" value={pollIntervalMs} onChange={event => setPollIntervalMs(Number(event.target.value) as (typeof POLL_INTERVAL_OPTIONS)[number])}>
              {POLL_INTERVAL_OPTIONS.map(option => (
                <option key={option} value={option}>{option} ms</option>
              ))}
            </select>
          </div>
          {error && <span className="adv-msg is-error" style={{ marginBottom: 0 }}>{error}</span>}
        </div>
      </div>

      <div className="uf-kpi-grid">
        {[
          { label: 'Dispatch bursts', value: String(overlaySummary.dispatch), tone: 'var(--accent)' },
          { label: 'Nonce bursts', value: String(overlaySummary.nonce), tone: 'var(--accent-orange)' },
          { label: 'Accepted shares', value: String(overlaySummary.accepted), tone: 'var(--green)' },
          { label: 'Rejected shares', value: String(overlaySummary.rejected), tone: 'var(--red)' },
          { label: 'Lucky shares', value: String(overlaySummary.lucky), tone: 'var(--yellow)' },
        ].map(card => (
          <div key={card.label} className="glass-card uf-kpi">
            <span className="sf-kpi-label">{card.label}</span>
            <span className="sf-kpi-value" style={{ color: card.tone }}>{card.value}</span>
          </div>
        ))}
      </div>

      <div className="sf-chain-list">
        {visibleChains.map(({ chainId, latest, history }) => {
          const statSeries: ChartSeries[] = [
            {
              data: history.map(point => ({ time: point.timestamp / 1000, value: point.flags.workTxFull ? 1 : 0 })),
              color: 'var(--yellow)',
              label: 'TX full',
            },
            {
              data: history.map(point => ({ time: point.timestamp / 1000, value: point.flags.workRxEmpty ? 0 : 1 })),
              color: 'var(--accent)',
              label: 'RX active',
            },
            {
              data: history.map(point => ({ time: point.timestamp / 1000, value: point.flags.irqPending ? 1 : 0 })),
              color: 'var(--accent-orange)',
              label: 'IRQ',
            },
          ];
          const workSeries: ChartSeries[] = [
            {
              data: history.map(point => ({ time: point.timestamp / 1000, value: point.workId })),
              color: 'var(--accent)',
              label: 'Work ID',
            },
            {
              data: history.map(point => ({ time: point.timestamp / 1000, value: point.errCnt })),
              color: 'var(--red)',
              label: 'ERR_CNT',
              dashed: true,
              yAxis: 'right',
            },
          ];

          // SR summaries derived from the exact series each chart renders, so
          // screen-reader truth tracks the visible FIFO/work telemetry.
          const sampleCount = history.length;
          const statSummary = sampleCount === 0
            ? `Chain ${chainId} FIFO flags chart, no samples yet`
            : `Chain ${chainId} FIFO flags over ${sampleCount} samples: latest ${latest ? (latest.flags.workTxFull ? 'TX full' : latest.flags.workTxEmpty ? 'TX idle' : 'TX feeding') + ', ' + (latest.flags.workRxEmpty ? 'RX empty' : 'RX active') + ', ' + (latest.flags.irqPending ? 'IRQ pending' : 'IRQ clear') : 'unknown'}`;
          const workSummary = sampleCount === 0
            ? `Chain ${chainId} work and error trend chart, no samples yet`
            : `Chain ${chainId} work and error trend over ${sampleCount} samples: latest work ID ${latest ? latest.workId : 0}, error count ${latest ? latest.errCnt : 0}`;

          return (
            <div key={chainId} className="register-inspector uf-chain">
              <div className="sf-chain-head">
                <div>
                  <div className="sf-chain-name">Chain {chainId}</div>
                  <div className="sf-chain-persona">
                    {latest ? `Last poll ${formatTime(latest.timestamp)}` : 'No samples yet'}
                  </div>
                </div>
                {latest && (
                  <div className="uf-chip-row">
                    <span className={`hacker-status-chip ${latest.flags.workTxFull ? 'warning' : latest.flags.workTxEmpty ? 'neutral' : 'success'}`}>{latest.flags.workTxFull ? 'TX full' : latest.flags.workTxEmpty ? 'TX idle' : 'TX feeding'}</span>
                    <span className={`hacker-status-chip ${latest.flags.workRxEmpty ? 'neutral' : 'success'}`}>{latest.flags.workRxEmpty ? 'RX empty' : 'RX active'}</span>
                    <span className={`hacker-status-chip ${latest.flags.irqPending ? 'info' : 'neutral'}`}>{latest.flags.irqPending ? 'IRQ pending' : 'IRQ clear'}</span>
                  </div>
                )}
              </div>

              {latest && (
                <div className="uf-metric-grid">
                  {[
                    { label: 'CTRL', value: formatHex(latest.ctrl, 8), tone: 'var(--text)' },
                    { label: 'BAUD', value: `${formatHex(latest.baud, 8)} · ${formatBaud(latest.baud)}`, tone: 'var(--accent)' },
                    { label: 'STAT', value: formatHex(latest.stat, 8), tone: 'var(--accent-orange)' },
                    { label: 'WORK_ID', value: String(latest.workId), tone: 'var(--accent)' },
                    { label: 'WORK_TIME', value: formatHex(latest.workTime, 8), tone: 'var(--text)' },
                    { label: 'ERR_CNT', value: String(latest.errCnt), tone: latest.errCnt > 0 ? 'var(--yellow)' : 'var(--green)' },
                    { label: 'TICKET_MASK', value: formatHex(latest.ticketMask, 8), tone: 'var(--text)' },
                    { label: 'HASH_CNT', value: `${latest.hashCntHi.toLocaleString()}:${latest.hashCntLo.toLocaleString()}`, tone: 'var(--accent-orange)' },
                  ].map(metric => (
                    <div key={metric.label} className="glass-card uf-metric">
                      <span className="uf-metric-label">{metric.label}</span>
                      <span className="uf-metric-value" style={{ color: metric.tone }}>{metric.value}</span>
                    </div>
                  ))}
                </div>
              )}

              <div className="uf-chart-grid">
                <div className="glass-card uf-chart-card">
                  <div className="uf-chart-title">FIFO Flags</div>
                  <SvgChart
                    series={statSeries}
                    height={180}
                    showYAxis={false}
                    formatValue={value => value > 0.5 ? '1' : '0'}
                    summaryText={statSummary}
                    style={{ borderRadius: 'var(--radius)', overflow: 'hidden' }}
                  />
                </div>

                <div className="glass-card uf-chart-card">
                  <div className="uf-chart-title">Work / Error Trend</div>
                  <SvgChart
                    series={workSeries}
                    height={180}
                    summaryText={workSummary}
                    style={{ borderRadius: 'var(--radius)', overflow: 'hidden' }}
                  />
                </div>
              </div>
            </div>
          );
        })}
      </div>

      <div className="register-inspector adv-card-mt-12" style={{ marginTop: 16 }}>
        <div className="uf-overlay-title">Mining-Sync Overlay</div>
        <div className="console-output uf-overlay-log">
          {overlays.length === 0 ? (
            <div className="uf-overlay-empty">Waiting for mining-sync events...</div>
          ) : overlays.slice().reverse().map((overlay, index) => (
            <div key={`${overlay.timestamp}-${index}`} className="uf-overlay-row">
              <span className="uf-overlay-ts">{formatTime(overlay.timestamp)}</span>
              <span style={{ color: overlay.event === 'share-' ? 'var(--red)' : overlay.event === 'lucky' ? 'var(--yellow)' : 'var(--accent)' }}>
                [{overlay.event}]
              </span>
              <span className="uf-overlay-detail">
                {overlay.detail}
                {overlay.chainId != null ? ` · chain ${overlay.chainId}` : ''}
              </span>
            </div>
          ))}
        </div>
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>focus: {focusChain === 'all' ? 'all chains' : `chain ${focusChain}`}</span>
          <span>{overlaySummary.dispatch} dispatch</span>
          <span>{overlaySummary.nonce} nonces</span>
          <span>{overlaySummary.accepted} accepted</span>
        </div>
      </footer>
    </div>
  );
}
