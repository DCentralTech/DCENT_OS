import React, { useMemo, useState } from 'react';
import { useFlightRecorder } from '../../hooks/useFlightRecorder';

function formatTime(timestamp: number) {
  return new Date(timestamp).toLocaleTimeString('en-US', {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

function formatDurationMs(startedAt: number, lastUpdatedAt: number) {
  const seconds = Math.max(0, Math.round((lastUpdatedAt - startedAt) / 1000));
  const minutes = Math.floor(seconds / 60);
  const remSeconds = seconds % 60;
  return `${minutes}m ${remSeconds}s`;
}

export function FlightRecorderPanel() {
  const {
    frozen,
    startedAt,
    lastUpdatedAt,
    totalCaptured,
    entries,
    freeze,
    resume,
    clear,
    exportBundle,
    addMarker,
    recordAction,
  } = useFlightRecorder();
  const [markerLabel, setMarkerLabel] = useState('');

  const recentEntries = useMemo(() => entries.slice(-60).reverse(), [entries]);
  const summary = useMemo(() => {
    let ws = 0;
    let actions = 0;
    let markers = 0;
    let miningSync = 0;
    let logs = 0;

    for (const entry of entries) {
      if (entry.source === 'ws') ws += 1;
      if (entry.source === 'action') actions += 1;
      if (entry.source === 'marker') markers += 1;
      if (entry.event.startsWith('mining_sync:')) miningSync += 1;
      if (entry.event.startsWith('log:')) logs += 1;
    }

    return { ws, actions, markers, miningSync, logs };
  }, [entries]);

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// flight recorder</div>
          <h2 className="hacker-inspector-title">Evidence Capture Buffer</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${frozen ? 'warning' : ''}`}>{frozen ? 'FROZEN' : 'RECORDING'}</span>
          <button className="hacker-inspector-help" onClick={() => {
            if (frozen) {
              resume();
              recordAction('flight_recorder_resumed');
            } else {
              freeze();
              recordAction('flight_recorder_frozen');
            }
          }}>
            {frozen ? 'RESUME' : 'FREEZE'}
          </button>
          <button className="hacker-inspector-help" onClick={() => clear()}>CLEAR</button>
          <button className="hacker-inspector-refresh" onClick={() => {
            recordAction('flight_recorder_exported', { frozen, entries: entries.length });
            exportBundle();
          }}>⤓ EXPORT</button>
        </div>
      </header>

      <div className="hacker-inspector-body">
      <div className="adv-stat-grid is-min-220 is-mb">
        {[
          { label: 'Window', value: '5 minutes', tone: 'var(--accent)' },
          { label: 'Elapsed', value: formatDurationMs(startedAt, lastUpdatedAt), tone: 'var(--accent-orange)' },
          { label: 'Visible entries', value: String(entries.length), tone: 'var(--text)' },
          { label: 'Captured total', value: String(totalCaptured), tone: 'var(--accent)' },
          { label: 'Mining sync', value: String(summary.miningSync), tone: 'var(--accent-orange)' },
          { label: 'Log events', value: String(summary.logs), tone: 'var(--yellow)' },
        ].map(card => (
          <div key={card.label} className="glass-card adv-stat-card ds-card-hover">
            <span className="adv-stat-label">{card.label}</span>
            <span className="adv-stat-value" style={{ color: card.tone }}>{card.value}</span>
          </div>
        ))}
      </div>

      <div className="fr-split">
        <section className="register-inspector ds-card-hover">
          <div className="adv-section-eyebrow">
            Marker Pad
          </div>
          <div className="adv-kv-stack is-gap-10">
            <input
              type="text"
              value={markerLabel}
              onChange={event => setMarkerLabel(event.target.value)}
              placeholder="Describe what just happened"
            />
            <button
              className="btn btn-secondary"
              onClick={() => {
                const label = markerLabel.trim() || 'manual marker';
                addMarker(label);
                recordAction('flight_recorder_marker_added', { label });
                setMarkerLabel('');
              }}
            >
              Add Marker
            </button>
            <div className="fr-marker-hint">
              Good markers: <code>fan 2 started oscillating</code>, <code>pool switched to donation</code>, <code>manual register write</code>, <code>heard strange relay click</code>.
            </div>
          </div>

          <div className="adv-section-eyebrow fr-mix-eyebrow">
            Capture Mix
          </div>
          <div className="adv-kv-stack fr-mix-stack">
            <div className="adv-kv-row">
              <span className="adv-kv-k">WebSocket events</span>
              <span className="adv-kv-v">{summary.ws}</span>
            </div>
            <div className="adv-kv-row">
              <span className="adv-kv-k">Action trail</span>
              <span className="adv-kv-v">{summary.actions}</span>
            </div>
            <div className="adv-kv-row">
              <span className="adv-kv-k">Markers</span>
              <span className="adv-kv-v is-orange">{summary.markers}</span>
            </div>
          </div>
        </section>

        <section className="register-inspector ds-card-hover">
          <div className="adv-section-eyebrow">
            Recent Timeline
          </div>
          <div className="console-output fr-timeline">
            {recentEntries.length === 0 ? (
              <div className="adv-state is-inline">No evidence captured yet.</div>
            ) : recentEntries.map(entry => (
              <div key={entry.id} className="fr-entry">
                <span className="fr-entry-time">{formatTime(entry.timestamp)}</span>
                <span style={{ color: entry.source === 'marker' ? 'var(--accent-orange)' : entry.source === 'action' ? 'var(--yellow)' : 'var(--accent)' }}>
                  [{entry.source}]
                </span>
                <span>
                  <strong className="fr-entry-event">{entry.event}</strong>
                  <span className="fr-entry-detail"> {JSON.stringify(entry.detail)}</span>
                </span>
              </div>
            ))}
          </div>
        </section>
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{entries.length} entries visible</span>
          <span>{totalCaptured} total captured</span>
          <span>elapsed {formatDurationMs(startedAt, lastUpdatedAt)}</span>
        </div>
      </footer>
    </div>
  );
}
