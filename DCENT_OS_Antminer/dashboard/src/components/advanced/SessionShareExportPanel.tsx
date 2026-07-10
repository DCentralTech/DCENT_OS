import React, { useMemo, useState } from 'react';
import { ActionButton } from '../common/ActionButton';
import { api } from '../../api/client';
import { useFlightRecorder } from '../../hooks/useFlightRecorder';
import { useMinerStore } from '../../store/miner';
import { redactSupportBundlePayload, SUPPORT_BUNDLE_REDACTED } from '../../utils/supportBundleRedaction';

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

export function SessionShareExportPanel() {
  const status = useMinerStore(s => s.status);
  const systemInfo = useMinerStore(s => s.systemInfo);
  const stats = useMinerStore(s => s.stats);
  const alerts = useMinerStore(s => s.alerts);
  const logEntries = useMinerStore(s => s.logEntries);
  const hashrateHistory = useMinerStore(s => s.hashrateHistory);
  const tempHistory = useMinerStore(s => s.tempHistory);
  const powerHistory = useMinerStore(s => s.powerHistory);
  const { entries, recordAction, addMarker } = useFlightRecorder();

  const [includeRecorder, setIncludeRecorder] = useState(true);
  const [includeLogs, setIncludeLogs] = useState(true);
  const [includeTelemetry, setIncludeTelemetry] = useState(true);
  const [includeShareHistory, setIncludeShareHistory] = useState(true);
  const [includeDiagnostics, setIncludeDiagnostics] = useState(true);
  const [exportState, setExportState] = useState<'idle' | 'running' | 'success' | 'error'>('idle');
  const [exportMessage, setExportMessage] = useState('');

  const summary = useMemo(() => ({
    recorderEntries: entries.length,
    logEntries: logEntries.length,
    openAlerts: alerts.filter(alert => !alert.dismissed).length,
    hashrateSamples: hashrateHistory.length,
  }), [alerts, entries.length, hashrateHistory.length, logEntries.length]);

  const buildBundle = async () => {
    setExportState('running');
    setExportMessage('Building export bundle...');
    addMarker('session share export');

    try {
      const [shareHistoryResult, reportsResult] = await Promise.allSettled([
        includeShareHistory ? api.getShareHistory() : Promise.resolve({ events: [] }),
        includeDiagnostics ? api.getRecentDiagnosticReports(8) : Promise.resolve({ status: 'ok', reports: [] }),
      ]);
      const shareHistory = shareHistoryResult.status === 'fulfilled'
        ? shareHistoryResult.value
        : { events: [] };
      const reports = reportsResult.status === 'fulfilled'
        ? reportsResult.value
        : { status: 'error', reports: [] as never[] };
      const shareEvents = shareHistory.events ?? [];
      const diagnosticReports = reports.reports ?? [];

      const bundle = redactSupportBundlePayload({
        exportedAt: new Date().toISOString(),
        redaction: {
          applied: true,
          placeholder: SUPPORT_BUNDLE_REDACTED,
          scope: [
            'object fields ending in password/token/secret/authorization/api key',
            'Authorization headers',
            'Bearer/Basic tokens',
            'credential-bearing URLs',
            'inline password/token/secret/api-key fragments in log text',
          ],
        },
        summary: {
          statusLoaded: Boolean(status),
          recorderEntries: includeRecorder ? entries.length : 0,
          shareEvents: shareEvents.length,
          reports: diagnosticReports.length,
        },
        current: includeTelemetry
          ? {
              status,
              systemInfo,
              stats,
              alerts,
              hashrateHistory,
              tempHistory,
              powerHistory,
            }
          : undefined,
        logs: includeLogs ? logEntries.slice(-200) : undefined,
        recorder: includeRecorder ? entries : undefined,
        shareHistory: includeShareHistory ? shareEvents : undefined,
        diagnostics: includeDiagnostics ? diagnosticReports : undefined,
        warnings: [
          shareHistoryResult.status === 'rejected' ? 'Share history endpoint unavailable during export.' : null,
          reportsResult.status === 'rejected' ? 'Recent diagnostics endpoint unavailable during export.' : null,
        ].filter(Boolean),
      });

      downloadJson(`dcentos-session-share-export-${Date.now()}.json`, bundle);
      setExportState('success');
      setExportMessage(`Exported redacted bundle with ${shareEvents.length} share events.`);
      recordAction('session_share_exported', {
        includeRecorder,
        includeLogs,
        includeTelemetry,
        includeShareHistory,
        includeDiagnostics,
        shareEvents: shareEvents.length,
      });
    } catch (error: unknown) {
      const message = error instanceof Error ? error.message : 'Failed to export session bundle';
      setExportState('error');
      setExportMessage(message);
      recordAction('session_share_export_failed', { message });
    }
  };

  const exportShareHistoryOnly = async () => {
    setExportState('running');
    setExportMessage('Exporting recent share history...');
    try {
      const history = await api.getShareHistory();
      const historyEvents = history.events ?? [];
      downloadJson(`dcentos-share-history-${Date.now()}.json`, redactSupportBundlePayload({
        exportedAt: new Date().toISOString(),
        events: historyEvents,
      }));
      setExportState('success');
      setExportMessage(`Exported ${historyEvents.length} recent share events.`);
      recordAction('share_history_exported', { count: historyEvents.length });
    } catch (error: unknown) {
      const message = error instanceof Error ? error.message : 'Failed to export share history';
      setExportState('error');
      setExportMessage(message);
      recordAction('share_history_export_failed', { message });
    }
  };

  const statusTone = exportState === 'error' ? 'danger' : exportState === 'success' ? '' : exportState === 'running' ? 'warning' : 'neutral';
  const statusLabel = exportState === 'idle' ? 'READY' : exportState.toUpperCase();

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// session share export</div>
          <h2 className="hacker-inspector-title">Support Bundle Builder</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${statusTone}`}>{statusLabel}</span>
          <button className="hacker-inspector-help" onClick={() => { void exportShareHistoryOnly(); }} disabled={exportState === 'running'}>
            SHARES
          </button>
          {/* Wave-13: removed the header "⤓ EXPORT" button — it duplicated the
              footer "Export Support Bundle" action (both call buildBundle); the
              footer one keeps the confirm dialog, which is the safer surface for
              a support bundle. The unique SHARES (history-only) export stays. */}
        </div>
      </header>

      <div className="hacker-inspector-body">
      <div className="adv-stat-grid is-min-220 is-mb">
        {[
          { label: 'Current hashrate', value: status ? `${(status.hashrate_ghs / 1000).toFixed(2)} TH/s` : 'n/a', tone: 'var(--accent)' },
          { label: 'Open alerts', value: String(summary.openAlerts), tone: summary.openAlerts > 0 ? 'var(--yellow)' : 'var(--green)' },
          { label: 'Recorder entries', value: String(summary.recorderEntries), tone: 'var(--accent-orange)' },
          { label: 'Log entries', value: String(summary.logEntries), tone: 'var(--text)' },
        ].map(card => (
          <div key={card.label} className="glass-card adv-stat-card ds-card-hover">
            <span className="adv-stat-label">{card.label}</span>
            <span className="adv-stat-value" style={{ color: card.tone }}>{card.value}</span>
          </div>
        ))}
      </div>

      <div className="register-inspector ds-card-hover adv-mb-16">
        <div className="sse-section-title">Bundle Sections</div>
        <div className="sse-opts">
          <label className="control-option"><input type="checkbox" checked={includeRecorder} onChange={event => setIncludeRecorder(event.target.checked)} /> Recorder timeline</label>
          <label className="control-option"><input type="checkbox" checked={includeLogs} onChange={event => setIncludeLogs(event.target.checked)} /> Recent logs</label>
          <label className="control-option"><input type="checkbox" checked={includeTelemetry} onChange={event => setIncludeTelemetry(event.target.checked)} /> Current telemetry</label>
          <label className="control-option"><input type="checkbox" checked={includeShareHistory} onChange={event => setIncludeShareHistory(event.target.checked)} /> Share history</label>
          <label className="control-option"><input type="checkbox" checked={includeDiagnostics} onChange={event => setIncludeDiagnostics(event.target.checked)} /> Recent diagnostics</label>
        </div>
      </div>

      <div className="register-inspector ds-card-hover">
        <div className={`sse-msg${exportState === 'error' ? ' is-error' : exportState === 'success' ? ' is-success' : ''}`}>
          {exportMessage || 'Build a redacted browser-session bundle when handing off a bug, share pattern, or thermal anomaly to another engineer.'}
        </div>
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{summary.recorderEntries} recorder entries</span>
          <span>{summary.logEntries} log entries</span>
          <span>{summary.openAlerts} open alerts</span>
        </div>
        <div className="hacker-inspector-actions-bottom">
          <ActionButton
            label={exportState === 'running' ? 'Building...' : 'Export Support Bundle'}
            onClick={buildBundle}
            confirm="Export a support/debug bundle from the current session?"
            disabled={exportState === 'running'}
          />
        </div>
      </footer>
    </div>
  );
}
