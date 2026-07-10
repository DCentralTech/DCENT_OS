// KitStatsKpiGrid — structural recreation of the design-kit's
// `DashboardPage.jsx` StatsKpiGrid section.
//
// Kit reference: ui_kits/dashboard/DashboardPage.jsx (StatsKpiGrid):
//   <div className="section">
//     <div className="section-title">Live Metrics <span className="small-tag">…</span></div>
//     <div className="kpi-grid">
//       8× <div className="kpi-card"><div className="kpi-label"/><div className="kpi-value"/></div>
//     </div>
//   </div>
//
// Kit values are fabricated. Here every cell is fed from the REAL store
// (status / stats / hashrate history / health). Truth contracts preserved:
// "connecting" ≠ "connected"; zero/no data ⇒ honest placeholder, never a
// fabricated number.
import React, { useCallback, useMemo } from 'react';
import { useMinerStore } from '../../store/miner';
import { useDashboardHealth } from '../../hooks/useDashboardHealth';
import { formatHashrateShort } from '../../utils/format';
import { glossaryText } from '../../utils/glossary';
import { useFxPulse, useRewardFx } from '../../fx/useRewardFx';

function KpiCell({
  label,
  value,
  unit,
  tone,
  accent,
  live,
  tipKey,
  big,
  fxClassName,
}: {
  label: string;
  value: string;
  unit?: string;
  tone?: 'good' | 'bad' | 'warn';
  accent?: boolean;
  live?: boolean;
  tipKey?: string;
  big?: boolean;
  fxClassName?: string;
}) {
  return (
    <div
      className={`kpi-card dcfx-contained ${accent ? 'accent' : ''} ${fxClassName ?? ''}`}
      data-tip={tipKey ? glossaryText(tipKey) : undefined}
      data-tooltip={tipKey ? glossaryText(tipKey) : undefined}
    >
      <div className="kpi-label">{label}</div>
      <div
        className={`kpi-value ${tone ?? ''} ${accent ? 'accent' : ''}`}
        style={big ? { fontSize: '1.2rem' } : undefined}
      >
        {live && <span className="live-dot" aria-hidden="true" />}
        {value}
        {unit && <span className="unit">{unit}</span>}
      </div>
      <span className="dcfx-kpi-underline" aria-hidden="true" />
    </div>
  );
}

export function KitStatsKpiGrid() {
  const status = useMinerStore(s => s.status);
  const stats = useMinerStore(s => s.stats);
  const hashrateHistory = useMinerStore(s => s.hashrateHistory);
  const health = useDashboardHealth();

  const hasStatus = status != null;
  const live = health.hasFreshTelemetry;
  const hashrate = status?.hashrate_ghs ?? 0;
  const accepted = status?.accepted ?? 0;
  const rejected = status?.rejected ?? 0;
  const total = accepted + rejected;
  const rejectRate = total > 0 ? (rejected / total) * 100 : 0;

  const { hr1m, hr15m, hr24h } = useMemo(() => {
    const now = Date.now() / 1000;
    const avg = (secs: number) => {
      const cut = now - secs;
      const pts = hashrateHistory.filter(p => p.time >= cut);
      if (pts.length === 0) return hashrate;
      return pts.reduce((s, p) => s + p.value, 0) / pts.length;
    };
    return { hr1m: avg(60), hr15m: avg(900), hr24h: avg(86400) };
  }, [hashrateHistory, hashrate]);

  const f1m = formatHashrateShort(hr1m);
  const f15m = formatHashrateShort(hr15m);
  const f24h = formatHashrateShort(hr24h);

  const poolStatus = (status?.pool?.status ?? '').toLowerCase();
  const poolConnected = poolStatus === 'connected' || poolStatus === 'active'
    || poolStatus === 'alive' || poolStatus === 'donating';
  const poolDisplay = !hasStatus || !health.hasRecentTelemetry
    ? 'Offline'
    : poolConnected
      ? 'Connected'
      : poolStatus === 'disconnected' || poolStatus === 'dead'
        ? 'Disconnected'
        : 'Waiting';

  const chainCount = stats?.chains?.length ?? status?.chains?.length ?? 0;
  const activeChains = (stats?.chains ?? status?.chains ?? [])
    .filter(c => (c.status ?? '').toLowerCase() === 'active').length;
  const tunerDisplay = !hasStatus || !health.hasRecentTelemetry
    ? 'Offline'
    : !live
      ? 'Standby'
      : chainCount > 0 && activeChains === chainCount
        ? 'Stable'
        : chainCount > 0
          ? `${activeChains}/${chainCount}`
          : 'Init';
  const [acceptedFx, pulseAccepted] = useFxPulse(650);
  const [rejectedFx, pulseRejected] = useFxPulse(450);

  useRewardFx(useCallback((event) => {
    if (event.intensity <= 0) return;
    if (event.kind === 'share-accepted') {
      pulseAccepted();
    } else if (event.kind === 'share-rejected') {
      pulseRejected();
    }
  }, [pulseAccepted, pulseRejected]));

  return (
    <div className="section" data-testid="kit-stats-kpi-grid">
      <div className="section-title">
        Live Metrics <span className="small-tag">rolling windows</span>
      </div>
      <div className="kpi-grid">
        <KpiCell
          label="Rejection rate"
          value={total > 0 ? rejectRate.toFixed(1) : hasStatus ? '0' : '—'}
          unit="%"
          tone={rejectRate > 5 ? 'bad' : rejectRate > 0 ? 'warn' : total > 0 ? 'good' : undefined}
          tipKey="share_rejected"
        />
        <KpiCell
          label="Hashrate 1m"
          value={hasStatus ? f1m.value : '—'}
          unit={hasStatus ? f1m.unit : undefined}
          accent
          tipKey="hashrate_local_vs_pool"
        />
        <KpiCell label="Hashrate 15m" value={hasStatus ? f15m.value : '—'} unit={hasStatus ? f15m.unit : undefined} />
        <KpiCell label="Hashrate 24h" value={hasStatus ? f24h.value : '—'} unit={hasStatus ? f24h.unit : undefined} />
        <KpiCell
          label="Accepted"
          value={hasStatus ? accepted.toLocaleString() : '—'}
          tone={accepted > 0 ? 'good' : undefined}
          tipKey="share_accepted"
          fxClassName={acceptedFx ? 'dcfx-share-flash' : undefined}
        />
        <KpiCell
          label="Rejected total"
          value={hasStatus ? rejected.toLocaleString() : '—'}
          tone={rejected > 0 ? 'warn' : undefined}
          tipKey="share_rejected"
          fxClassName={rejectedFx ? 'dcfx-share-reject' : undefined}
        />
        <KpiCell
          label="Tuner status"
          value={tunerDisplay}
          tone={tunerDisplay === 'Stable' ? 'good' : undefined}
          big
        />
        <KpiCell
          label="Pool status"
          value={poolDisplay}
          tone={poolDisplay === 'Connected' ? 'good' : poolDisplay === 'Offline' || poolDisplay === 'Disconnected' ? 'bad' : 'warn'}
          live={poolDisplay === 'Connected'}
          big
          tipKey="pool_state"
        />
      </div>
    </div>
  );
}
