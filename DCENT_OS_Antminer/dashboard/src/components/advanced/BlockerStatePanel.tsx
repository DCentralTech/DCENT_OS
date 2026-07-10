import { getLastSeenAgeMs, useSystemHealth } from '../common/proxy/SystemHealthContext';

export function BlockerStatePanel() {
  const { health, endpointAvailable, state } = useSystemHealth();

  // HACK-B-007: never render a blank pane. The common healthy native case (and
  // the genuinely-no-telemetry case) previously returned null, leaving an empty
  // void on every healthy native miner. Render an explicit, honest empty state.
  if (!endpointAvailable || !health || state === 'native' || state === 'unknown') {
    const empty =
      !endpointAvailable || !health
        ? {
            label: 'NO DATA',
            tone: 'neutral',
            note:
              "Blocker-state telemetry isn't available from this miner — the overlay health endpoint is only served when DCENT_OS runs in a gated proxy/overlay mode. Nothing to report.",
          }
        : state === 'native'
          ? {
              label: 'NO BLOCKERS',
              tone: '',
              note: 'No mining blockers — running native, nothing is gating nonces.',
            }
          : {
              label: 'UNKNOWN',
              tone: 'neutral',
              note: "The miner hasn't reported an overlay blocker state yet.",
            };
    return (
      <div className="hacker-inspector" aria-label="Overlay blocker state">
        <header className="hacker-inspector-header">
          <div className="hacker-inspector-title-group">
            <div className="hacker-inspector-eyebrow">// overlay blocker state</div>
            <h2 className="hacker-inspector-title">DCENT_OS Blocker Trace</h2>
          </div>
          <div className="hacker-inspector-actions">
            <span className={`hacker-inspector-status ${empty.tone}`}>{empty.label}</span>
          </div>
        </header>
        <div className="hacker-inspector-body">
          <div className="adv-empty-note">{empty.note}</div>
        </div>
      </div>
    );
  }

  const daemon = health.daemon;
  const bosminer = health.bosminer;
  const rail = health.rail;
  const scrape = health.scrape;
  const action = health.recovery?.next_action ?? null;
  const blockers = bosminer?.blockers?.length ? bosminer.blockers.join(', ') : 'none';
  const lastSeen = formatAge(getLastSeenAgeMs(health));

  const rows = [
    ['daemon', `alive${daemon?.pid ? ` pid=${daemon.pid}` : ''}${daemon?.uptime_s ? ` uptime=${formatUptime(daemon.uptime_s)}` : ''}`],
    ['mode', String(health.mode ?? 'unknown')],
    ['bosminer', bosminer ? `alive=${bosminer.alive ? 'true' : 'false'}${bosminer.pid ? ` pid=${bosminer.pid}` : ''} last=${lastSeen ?? 'never'}` : 'not reported'],
    ['blockers', blockers],
    ['cgminer', scrape ? `reachable=${scrape.cgminer_reachable ? 'true' : 'false'} failures=${scrape.consecutive_failures ?? 0}` : 'not reported'],
    ['rail verdict', rail ? String(rail.verdict) : 'not reported'],
    ['uart rx post enable', rail ? String(rail.uart_rx_bytes_post_enable ?? 0) : 'not reported'],
    // : relabeled "multimeter"/"unresolved" — the operator never takes a
    // physical reading (rail/electrical questions are resolved in software), and
    // "unresolved" read as a defect on every normal unit. "rail readback" + "—".
    ['rail readback', rail?.last_multimeter_reading_v != null ? `${rail.last_multimeter_reading_v.toFixed(2)} V` : '—'],
    ['recovery', action?.kind ?? 'not reported'],
  ];

  const tone = state === 'hardware_blocked' ? 'danger' : 'warning';

  return (
    <div className="hacker-inspector" aria-label="Overlay blocker state">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// overlay blocker state</div>
          <h2 className="hacker-inspector-title">DCENT_OS Blocker Trace</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${tone}`}>{String(state).toUpperCase()}</span>
        </div>
      </header>

      <div className="hacker-inspector-body">
        <div className="bsp-trace">
          {rows.map(([label, value]) => (
            <div key={label} className="bsp-row">
              <span className="bsp-k">{label}</span>
              <span style={{ color: label === 'blockers' && value !== 'none' ? '#EF4444' : 'var(--text)' }}>{value}</span>
            </div>
          ))}
        </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{rows.length} fields</span>
          <span>last seen {lastSeen ?? 'never'}</span>
        </div>
      </footer>
    </div>
  );
}

function formatAge(ageMs: number | null): string | null {
  if (ageMs == null) {
    return null;
  }
  const sec = Math.floor(ageMs / 1000);
  if (sec < 5) return 'just now';
  if (sec < 60) return `${sec}s ago`;
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m ago`;
  return `${Math.floor(min / 60)}h ${min % 60}m ago`;
}

function formatUptime(sec: number): string {
  if (sec < 60) return `${sec}s`;
  if (sec < 3600) return `${Math.floor(sec / 60)}m`;
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  return `${h}h${m > 0 ? ` ${m}m` : ''}`;
}

