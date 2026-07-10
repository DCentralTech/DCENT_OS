import React, { useMemo, useState } from 'react';
import { useFlightRecorder } from '../../hooks/useFlightRecorder';

type JournalFilter = 'all' | 'action' | 'nav' | 'marker';

function formatTime(timestamp: number) {
  return new Date(timestamp).toLocaleTimeString('en-US', {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
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

export function CommandJournalPanel() {
  const { entries } = useFlightRecorder();
  const [filter, setFilter] = useState<JournalFilter>('all');
  const [query, setQuery] = useState('');

  const journalEntries = useMemo(() => {
    return entries
      .filter(entry => entry.source === 'action' || entry.source === 'nav' || entry.source === 'marker')
      .filter(entry => filter === 'all' || entry.source === filter)
      .filter(entry => {
        if (!query.trim()) {
          return true;
        }
        const haystack = `${entry.event} ${JSON.stringify(entry.detail)}`.toLowerCase();
        return haystack.includes(query.toLowerCase());
      })
      .slice()
      .reverse();
  }, [entries, filter, query]);

  const summary = useMemo(() => {
    let actions = 0;
    let nav = 0;
    let markers = 0;
    for (const entry of entries) {
      if (entry.source === 'action') actions += 1;
      if (entry.source === 'nav') nav += 1;
      if (entry.source === 'marker') markers += 1;
    }
    return { actions, nav, markers };
  }, [entries]);

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// command journal</div>
          <h2 className="hacker-inspector-title">Operator Audit Trail</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className="hacker-inspector-status">{journalEntries.length} ENTRIES</span>
          <button
            className="hacker-inspector-refresh"
            onClick={() => downloadJson(`dcentos-command-journal-${Date.now()}.json`, {
              exportedAt: new Date().toISOString(),
              entries: journalEntries,
            })}
          >
            ⤓ EXPORT
          </button>
        </div>
      </header>

      <div className="hacker-inspector-toolbar">
        <input
          type="text"
          value={query}
          onChange={event => setQuery(event.target.value)}
          placeholder="Search event name or detail"
          className="cj-search"
        />
        <div className="tab-bar cj-tabbar">
          {(['all', 'action', 'nav', 'marker'] as JournalFilter[]).map(value => (
            <button
              key={value}
              className={`tab ${filter === value ? 'active' : ''}`}
              onClick={() => setFilter(value)}
            >
              {value}
            </button>
          ))}
        </div>
      </div>

      <div className="hacker-inspector-body">
      <div className="adv-stat-grid is-min-220 is-mb">
        {[
          { label: 'Actions', value: String(summary.actions), tone: 'var(--accent)' },
          { label: 'Navigation', value: String(summary.nav), tone: 'var(--accent-orange)' },
          { label: 'Markers', value: String(summary.markers), tone: 'var(--yellow)' },
          { label: 'Visible', value: String(journalEntries.length), tone: 'var(--text)' },
        ].map(card => (
          <div key={card.label} className="glass-card adv-stat-card ds-card-hover">
            <span className="adv-stat-label">{card.label}</span>
            <span className="adv-stat-value" style={{ color: card.tone }}>{card.value}</span>
          </div>
        ))}
      </div>

      <div className="register-inspector ds-card-hover">
        <div className="console-output cj-stream">
          {journalEntries.length === 0 ? (
            <div className="adv-state is-inline">No journal entries match the current filter.</div>
          ) : journalEntries.map(entry => (
            <div key={entry.id} className="cj-row">
              <span className="cj-time">{formatTime(entry.timestamp)}</span>
              <span style={{ color: entry.source === 'action' ? 'var(--accent)' : entry.source === 'nav' ? 'var(--accent-orange)' : 'var(--yellow)' }}>
                [{entry.source}]
              </span>
              <span>
                <strong className="cj-event">{entry.event}</strong>
                <span className="cj-detail"> {JSON.stringify(entry.detail)}</span>
              </span>
            </div>
          ))}
        </div>
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{summary.actions} actions</span>
          <span>{summary.nav} navs</span>
          <span>{summary.markers} markers</span>
          <span>filter: {filter}</span>
        </div>
      </footer>
    </div>
  );
}
