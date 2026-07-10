import React, { useState, useRef, useEffect, useCallback } from 'react';
import { api } from '../../api/client';
import type { LogManifestResponse } from '../../api/types';
import { useMinerStore, type LogEntry } from '../../store/miner';
import { useWindowedList } from '../../hooks/useWindowedList';
import { StatePanel } from '../common/StatePanel';
import { EmptyState } from '../common/EmptyState';
import { NoLogsIllustration } from '../common/illustrations';
import { SectionSkeleton } from '../common/skeletons';

const LEVEL_COLORS: Record<LogEntry['level'], string> = {
  info: 'var(--text-secondary)',
  warn: 'var(--yellow)',
  error: 'var(--red)',
  debug: 'var(--text-dim)',
};

const LEVEL_LABELS: Record<LogEntry['level'], string> = {
  info: 'INF',
  warn: 'WRN',
  error: 'ERR',
  debug: 'DBG',
};

type FilterLevel = 'all' | LogEntry['level'];

function formatManifestSize(bytes: number | null) {
  if (bytes == null || !Number.isFinite(bytes)) return 'unknown size';
  if (bytes < 1024) return `${bytes} B`;
  const kib = bytes / 1024;
  if (kib < 1024) return `${kib.toFixed(kib >= 10 ? 0 : 1)} KiB`;
  const mib = kib / 1024;
  return `${mib.toFixed(mib >= 10 ? 0 : 1)} MiB`;
}

function formatManifestTime(ms: number | null) {
  if (ms == null || !Number.isFinite(ms)) return 'unknown modified time';
  const date = new Date(ms);
  return Number.isNaN(date.getTime()) ? 'unknown modified time' : date.toLocaleString();
}

function formatContentAccess(access: string) {
  switch (access) {
    case 'mode_gated_content_endpoint':
      return 'Mode-gated content endpoint';
    case 'not_exposed_metadata_only':
      return 'Metadata only';
    default:
      return access.replace(/_/g, ' ');
  }
}

// Parse a dcentrald log line like: "2026-03-24T01:23:45Z  INFO dcentrald::daemon: message chain_id=6"
function parseLogLine(line: string, id: number): LogEntry | null {
  const trimmed = line.replace(/\x1B\[[0-9;]*m/g, '').trim();
  if (!trimmed) return null;

  let level: LogEntry['level'] = 'info';
  if (trimmed.includes(' WARN') || trimmed.includes(' WARN ')) level = 'warn';
  else if (trimmed.includes(' ERROR') || trimmed.includes('ERROR ')) level = 'error';
  else if (trimmed.includes(' DEBUG') || trimmed.includes('TRACE')) level = 'debug';

  let source: LogEntry['source'] = 'system';
  if (trimmed.includes('work_dispatcher') || trimmed.includes('stratum') || trimmed.includes('mining')) {
    source = 'mining';
  }

  const msgMatch = trimmed.match(/(?:dcentrald\S*:\s*)(.*)/);
  const message = msgMatch ? msgMatch[1] : trimmed;

  return { id, timestamp: Date.now(), level, source, message };
}

export function LogsPage() {
  const logEntries = useMinerStore(s => s.logEntries);
  const wsConnected = useMinerStore(s => s.wsConnected);
  const [filter, setFilter] = useState<FilterLevel>('all');
  const [autoScroll, setAutoScroll] = useState(true);
  const [restLogs, setRestLogs] = useState<LogEntry[]>([]);
  const [restLoading, setRestLoading] = useState(false);
  const [restError, setRestError] = useState('');
  const [manifest, setManifest] = useState<LogManifestResponse | null>(null);
  const [manifestError, setManifestError] = useState('');
  const fetchedRef = useRef(false);

  const fetchLogManifest = useCallback(async () => {
    setManifestError('');
    try {
      setManifest(await api.getLogManifest());
    } catch (error) {
      setManifest(null);
      setManifestError(error instanceof Error ? error.message : 'Log source manifest unavailable');
    }
  }, []);

  const fetchRestLogs = useCallback(async () => {
    setRestLoading(true);
    setRestError('');
    try {
      const data = await api.getDebugLog(200);
      const lines: string[] = data.lines || data.log || [];
      if (Array.isArray(lines)) {
        const parsed = lines
          .map((line, i) => parseLogLine(typeof line === 'string' ? line : JSON.stringify(line), i))
          .filter((entry): entry is LogEntry => entry !== null);
        setRestLogs(parsed);
      } else {
        setRestLogs([]);
        setRestError('Debug log endpoint returned an unsupported response shape.');
      }
    } catch (e: unknown) {
      setRestError(e instanceof Error ? e.message : 'Failed to fetch logs');
    } finally {
      setRestLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!wsConnected && logEntries.length === 0 && !fetchedRef.current) {
      fetchedRef.current = true;
      fetchRestLogs();
    }
  }, [wsConnected, logEntries.length, fetchRestLogs]);

  useEffect(() => {
    void fetchLogManifest();
  }, [fetchLogManifest]);

  const allLogs = logEntries.length > 0 ? logEntries : restLogs;
  const filtered = filter === 'all' ? allLogs : allLogs.filter(log => log.level === filter);
  const logWindow = useWindowedList<HTMLDivElement>({
    count: filtered.length,
    itemHeight: 42,
    overscan: 12,
    disabled: filtered.length <= 120,
  });
  const visibleLogs = filtered.slice(logWindow.start, logWindow.end);
  const miningCount = filtered.filter(log => log.source === 'mining').length;
  const systemCount = filtered.filter(log => log.source === 'system').length;
  const filterLabel = 'Log level filter';
  // : errorCount / transportLabel / transportTone removed with the
  // duplicate KPI grid — the hero strip already shows Errors-1h + transport.

  useEffect(() => {
    if (autoScroll && logWindow.containerRef.current) {
      logWindow.containerRef.current.scrollTop = logWindow.containerRef.current.scrollHeight;
    }
  }, [filtered, autoScroll, logWindow.containerRef]);

  const formatTime = (ts: number) => {
    const date = new Date(ts);
    return date.toLocaleTimeString('en-US', {
      hour12: false,
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    });
  };

  // Hero KPIs: 1h windowed buckets from current view
  const oneHourAgo = Date.now() - 60 * 60 * 1000;
  const recentBucket = filtered.filter(e => e.timestamp >= oneHourAgo);
  const errors1h = recentBucket.filter(e => e.level === 'error').length;
  const warns1h = recentBucket.filter(e => e.level === 'warn').length;
  const info1h = recentBucket.filter(e => e.level === 'info').length;
  // Approximate lines/sec from this bucket
  const linesPerSec = recentBucket.length > 0
    ? (recentBucket.length / 3600).toFixed(recentBucket.length >= 360 ? 1 : 2)
    : '0';
  const heroBadgeTone: 'good' | 'warn' = errors1h > 0 ? 'warn' : 'good';
  const heroBadgeLabel = errors1h > 0
    ? `${errors1h} error${errors1h !== 1 ? 's' : ''} 1h`
    : warns1h > 0 ? `${warns1h} warn 1h` : 'quiet';

  return (
    <div className="page-content">
      <div className="page-hero-strip">
        <div className="page-hero-summary">
          <div className="page-hero-eyebrow">DAEMON LOGS</div>
          <div className="page-hero-title">Logs And Events</div>
          <div className="page-hero-stat">{filtered.length.toLocaleString()}</div>
          <div className="page-hero-substat">
            {wsConnected ? 'Live WebSocket stream' : restLogs.length > 0 ? 'REST fallback active' : 'Awaiting log data'}
            {' · filter: '}{filter}
          </div>
        </div>
        <div className="hero-kpi-strip">
          <div className="hero-kpi">
            <div className="kpi-label">Errors 1h</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{errors1h.toLocaleString()}</span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Warnings 1h</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{warns1h.toLocaleString()}</span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Info 1h</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{info1h.toLocaleString()}</span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Lines / sec</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{linesPerSec}</span>
            </div>
          </div>
        </div>
      </div>

      <section className="section">
      <div className="page-toolbar" style={{ marginBottom: 12 }}>
        <div className="section-title" style={{ margin: 0 }}>
          System Logs
          <span className={`small-tag ${heroBadgeTone}`}>{heroBadgeLabel}</span>
        </div>
        <div className="page-toolbar-actions">
          {/* Kit LogsPage level tab bar — kit `.tab-underline` grammar
              (Pages.jsx All/Info/Warn/Error). Dual-classed with the
              production `time-range-tabs`/`time-tab` hooks so the handoff
              skin styles it as the kit while the existing wiring/aria
              is preserved verbatim. */}
          <div
            className="tab-underline time-range-tabs"
            role="group"
            aria-label={filterLabel}
          >
            {(['all', 'info', 'warn', 'error', 'debug'] as FilterLevel[]).map(level => (
              <button
                key={level}
                type="button"
                className={`time-tab ${filter === level ? 'active' : ''}`}
                onClick={() => setFilter(level)}
                aria-pressed={filter === level}
                aria-label={level === 'all' ? 'Show all log entries' : `Show ${level} log entries`}
              >
                {level === 'all' ? 'All' : level.charAt(0).toUpperCase() + level.slice(1)}
              </button>
            ))}
          </div>
          {!wsConnected && (
            <button
              className="btn btn-secondary btn-compact"
              type="button"
              onClick={() => { void fetchRestLogs(); }}
              disabled={restLoading}
              style={{
                cursor: restLoading ? 'wait' : 'pointer',
                opacity: restLoading ? 0.6 : 1,
              }}
            >
              {restLoading ? 'Loading...' : 'Refresh'}
            </button>
          )}
          <label className="control-option">
            <input
              type="checkbox"
              checked={autoScroll}
              onChange={event => setAutoScroll(event.target.checked)}
            />
            Auto-scroll
          </label>
        </div>
      </div>

      {!wsConnected && restLogs.length > 0 && (
        <StatePanel
          title="Using REST fallback"
          message="WebSocket is disconnected, so this page is showing logs from the read-only debug log endpoint. Use Refresh to pull the latest entries."
          tone="warning"
          compact
        />
      )}

      {!wsConnected && restError && (
        <StatePanel
          title="Log stream unavailable"
          message={restError}
          tone="warning"
          compact
        />
      )}

      {manifestError && (
        <StatePanel
          title="Log source manifest unavailable"
          message={manifestError}
          tone="warning"
          compact
        />
      )}

      {/* Wave-13: removed the 4-card KPI grid here — Visible Entries, Transport
          and Errors all duplicated the hero strip (visible count + transport
          substat + Errors/Warnings/Info 1h). The only unique datum, the
          mining-vs-system source split of the visible entries, is kept as a
          compact line. */}
      <div className="log-source-split">
        {miningCount.toLocaleString()} mining · {systemCount.toLocaleString()} system entries in view
      </div>

      {manifest && (
        <section className="perf-below-fold" style={{ marginBottom: 16 }}>
          <div className="section-title" style={{ marginBottom: 8 }}>Log Sources</div>
          <div style={{ color: 'var(--text-dim)', fontSize: '0.78rem', marginBottom: 10 }}>
            Read-only metadata manifest. No log content was collected by this endpoint.
          </div>
          <div style={{ display: 'grid', gap: 8 }}>
            {manifest.sources.map(source => (
              <div
                key={source.id}
                style={{
                  display: 'grid',
                  gridTemplateColumns: 'repeat(auto-fit, minmax(180px, 1fr))',
                  gap: 10,
                  alignItems: 'start',
                  padding: '10px 0',
                  borderTop: '1px solid var(--border)',
                  fontSize: '0.76rem',
                }}
              >
                <div>
                  <div style={{ color: 'var(--text)', fontWeight: 700 }}>{source.label}</div>
                  <div style={{ color: source.exists ? 'var(--green)' : 'var(--text-dim)' }}>
                    {source.metadata_status}
                  </div>
                </div>
                <div style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-dim)', wordBreak: 'break-all' }}>
                  {source.path}
                </div>
                <div style={{ color: 'var(--text-dim)' }}>
                  <div>{formatContentAccess(source.content_access)}</div>
                  <div>{formatManifestSize(source.size_bytes)}</div>
                  <div>{formatManifestTime(source.modified_ms)}</div>
                </div>
              </div>
            ))}
          </div>
          {manifest.limitations.length > 0 && (
            <div style={{ marginTop: 8, color: 'var(--text-dim)', fontSize: '0.72rem', lineHeight: 1.45 }}>
              {manifest.limitations.map(item => (
                <div key={item}>- {item}</div>
              ))}
            </div>
          )}
        </section>
      )}

      <div ref={logWindow.containerRef} onScroll={logWindow.onScroll} className="log-viewer">
        {filtered.length === 0 && (
          <div style={{ padding: 24 }}>
            {restError ? (
              <StatePanel
                title="Could not load logs"
                message={restError}
                tone="danger"
                compact
              />
            ) : restLoading ? (
              <SectionSkeleton rows={4} data-testid="logs-page-loading" />
            ) : (
              <EmptyState
                illustration={<NoLogsIllustration />}
                title="No logs to show"
                hint="No synthetic logs were generated. DCENT_OS will not create status-derived log rows when no log content is available."
                data-testid="logs-page-empty"
              />
            )}
          </div>
        )}
        {/* Kit log grammar: `.log-row` → `.log-ts` / `.log-lvl <LEVEL>` /
            `.log-src` / `.log-msg` (styles.css:518-531). The kit colour-codes
            via an uppercase level class on `.log-lvl`; we map our 4 levels
            (info/warn/error/debug) onto the kit's INFO/WARN/ERROR classes and
            keep the production `.log-col-*` cells + inline colour fallbacks. */}
        {logWindow.padTop > 0 && (
          <div aria-hidden="true" style={{ height: logWindow.padTop }} />
        )}
        {visibleLogs.map(entry => {
          const kitLvlClass =
            entry.level === 'warn' ? 'WARN'
            : entry.level === 'error' ? 'ERROR'
            : 'INFO';
          return (
          <div key={entry.id} className="log-row">
            <span className="log-ts log-col-time">
              {formatTime(entry.timestamp)}
            </span>
            <span className={`log-lvl ${kitLvlClass} log-col-level`} style={{
              color: LEVEL_COLORS[entry.level],
              fontWeight: entry.level === 'error' ? 700 : 400,
            }}>
              {LEVEL_LABELS[entry.level]}
            </span>
            <span className="log-src log-col-source">
              [{entry.source}]
            </span>
            <span className="log-msg log-col-message" style={{
              color: entry.level === 'error' ? 'var(--red)' : 'var(--text)',
            }}>
              {entry.message}
            </span>
          </div>
          );
        })}
        {logWindow.padBottom > 0 && (
          <div aria-hidden="true" style={{ height: logWindow.padBottom }} />
        )}
      </div>
      </section>
    </div>
  );
}
