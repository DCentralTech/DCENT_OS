import React from 'react';
import { useMinerStore } from '../../store/miner';

interface Anomaly {
  severity: 'critical' | 'warning';
  message: string;
}

export function AnomalyPanel() {
  const status = useMinerStore(s => s.status);
  const stats = useMinerStore(s => s.stats);

  const anomalies: Anomaly[] = [];

  if (status) {
    // Check each chain
    const chains = Array.isArray(status.chains) ? status.chains : [];
    chains.forEach(ch => {
      if (ch.chips === 0) {
        anomalies.push({ severity: 'critical', message: `Chain ${ch.id}: No chips detected` });
      }
      if (ch.temp_c > 65) {
        anomalies.push({ severity: 'warning', message: `Chain ${ch.id}: Chip temp ${ch.temp_c.toFixed(1)}\u00B0C (threshold: 65\u00B0C)` });
      }
    });

    // Pool
    const poolStatus = status.pool?.status?.toLowerCase();
    if (poolStatus === 'dead' || poolStatus === 'disconnected') {
      anomalies.push({ severity: 'critical', message: `Pool disconnected: ${status.pool?.url || 'unknown'}` });
    }

    // Reject rate
    const total = (status.accepted ?? 0) + (status.rejected ?? 0);
    if (total > 10) {
      const rejectPct = ((status.rejected ?? 0) / total) * 100;
      if (rejectPct > 5) {
        anomalies.push({ severity: 'warning', message: `Reject rate ${rejectPct.toFixed(1)}% (threshold: 5%)` });
      }
    }

    // Fan
    if (status.fans?.rpm === 0 && status.hashrate_ghs > 0) {
      anomalies.push({ severity: 'critical', message: 'Fan tachometer reading 0 RPM while mining active' });
    }
  }

  // HW errors from stats
  const totalHwErrors = (stats?.chains ?? []).reduce((s, c) => s + (c.errors ?? 0), 0);
  if (totalHwErrors > 0) {
    anomalies.push({ severity: 'warning', message: `${totalHwErrors} hardware error(s) detected` });
  }

  const criticalCount = anomalies.filter(a => a.severity === 'critical').length;
  const warningCount = anomalies.filter(a => a.severity === 'warning').length;
  const statusTone = criticalCount > 0 ? 'danger' : warningCount > 0 ? 'warning' : '';
  const statusLabel = anomalies.length === 0
    ? 'NOMINAL'
    : `${criticalCount} CRIT \u00B7 ${warningCount} WARN`;

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// anomaly detector</div>
          <h2 className="hacker-inspector-title">Live Anomaly Feed</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${statusTone}`}>{statusLabel}</span>
        </div>
      </header>

      <div className="hacker-inspector-body">
        {anomalies.length === 0 ? (
          <div className="anomaly-row anomaly-row-nominal">
            <span className="anomaly-row-dot" aria-hidden="true" />
            All systems nominal
          </div>
        ) : (
          <div className="anomaly-row-list">
            {anomalies.map((a, i) => (
              <div
                key={i}
                className={`anomaly-row anomaly-row-${a.severity}`}
              >
                <span className="anomaly-row-glyph" aria-hidden="true">{'\u26A0'}</span>
                {a.message}
              </div>
            ))}
          </div>
        )}
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{anomalies.length} anomaly entries</span>
          <span>{status?.chains?.length ?? 0} chains scanned</span>
        </div>
      </footer>
    </div>
  );
}
