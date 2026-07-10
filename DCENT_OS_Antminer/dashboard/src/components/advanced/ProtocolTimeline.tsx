import React, { useMemo, useState } from 'react';
import { useProtocolTrace } from '../../hooks/useProtocolTrace';

type LaneFilter = 'all' | 'pool' | 'protocol' | 'job' | 'dispatch' | 'nonce' | 'share' | 'system';

function formatTime(timestamp: number) {
  return new Date(timestamp).toLocaleTimeString('en-US', {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

function laneColor(lane: LaneFilter) {
  switch (lane) {
    case 'pool': return '#60A5FA';
    case 'protocol': return '#A78BFA';
    case 'job': return 'var(--accent-orange)';
    case 'dispatch': return '#2DD4A0';
    case 'nonce': return 'var(--accent)';
    case 'share': return '#F59E0B';
    case 'system': return 'var(--red)';
    default: return 'var(--text-dim)';
  }
}

function formatLiveWallPower(watts: number | null) {
  return watts != null ? `${watts.toFixed(0)} W` : 'Unavailable';
}

export function ProtocolTimeline() {
  const { events, snapshot, clearTimeline, exportTimeline } = useProtocolTrace();
  const [filter, setFilter] = useState<LaneFilter>('all');

  const filteredEvents = useMemo(
    () => events.filter(event => filter === 'all' || event.lane === filter).slice().reverse(),
    [events, filter],
  );

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// protocol timeline</div>
          <h2 className="hacker-inspector-title">Stratum Trace Stream</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${snapshot.sv2Connected ? '' : 'neutral'}`}>
            {snapshot.sv2Connected ? 'SV2 LINKED' : 'SV1 ACTIVE'}
          </span>
          <button className="hacker-inspector-help" onClick={clearTimeline} title="Clear timeline">CLEAR</button>
          <button className="hacker-inspector-refresh" onClick={exportTimeline} title="Export timeline JSON">⤓ EXPORT</button>
        </div>
      </header>

      <div className="hacker-inspector-toolbar">
        <div className="tab-bar pt-tabbar">
          {(['all', 'pool', 'protocol', 'job', 'dispatch', 'nonce', 'share', 'system'] as LaneFilter[]).map(lane => (
            <button
              key={lane}
              className={`tab ${filter === lane ? 'active' : ''}`}
              onClick={() => setFilter(lane)}
            >
              {lane}
            </button>
          ))}
        </div>
      </div>

      <div className="hacker-inspector-body">
      <div className="adv-stat-grid is-min-220 pt-stat-grid">
        {[
          { label: 'Pool', value: snapshot.poolStatus, tone: 'var(--accent)' },
          { label: 'Protocol', value: snapshot.protocolVersion || 'sv1', tone: 'var(--accent-orange)' },
          { label: 'Hashrate', value: `${(snapshot.hashrateGhs / 1000).toFixed(1)} TH/s`, tone: 'var(--text)' },
          { label: 'Power', value: formatLiveWallPower(snapshot.wallWatts), tone: 'var(--accent)' },
        ].map(card => (
          <div key={card.label} className="glass-card adv-stat-card ds-card-hover pt-stat-card">
            <span className="adv-stat-label">{card.label}</span>
            <span className="adv-stat-value" style={{ color: card.tone }}>{card.value}</span>
          </div>
        ))}
      </div>

      <div className="register-inspector ds-card-hover">
        <div className="console-output table-wrap pt-stream">
          {filteredEvents.length === 0 ? (
            <div className="adv-state is-inline">No protocol events captured for this filter yet.</div>
          ) : filteredEvents.map(event => (
            <div key={event.id} className="pt-row">
              <span className="pt-time">{formatTime(event.timestamp)}</span>
              <span className="pt-lane" style={{ color: laneColor(event.lane) }}>[{event.lane}]</span>
              <span>
                <strong style={{ color: event.severity === 'danger' ? 'var(--red)' : event.severity === 'warning' ? 'var(--yellow)' : 'var(--text)' }}>
                  {event.title}
                </strong>
                <span className="pt-detail"> {event.detail}</span>
                {event.tags.length > 0 && (
                  <span className="pt-tags"> {' '}| {event.tags.join(' · ')}</span>
                )}
              </span>
            </div>
          ))}
        </div>
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{events.length} events</span>
          <span>{filteredEvents.length} shown</span>
          <span>filter: {filter}</span>
        </div>
      </footer>
    </div>
  );
}
