import React from 'react';
import { useMinerStore } from '../../store/miner';
import { wattsToBtu } from '../../utils/thermal';
import { getDisplayPowerWatts, getLiveDisplayWallWatts } from '../../utils/power';
import { formatHashrateShort } from '../../utils/format';
import { glossaryText } from '../../utils/glossary';

/**
 * "The engine" panel — frames the miner as the heat engine behind the
 * warmth. Emits the kit `nest-engine` grammar (styled by
 * handoff-skin-heater.css) to match HeaterMode.jsx's `EnginePanel`.
 *
 * Every value is REAL store data — NEVER the kit's hardcoded "112.4 TH/s" /
 * "342 chips" / "Antminer S19j Pro". Honest "—" when a datum is absent:
 *
 *   - model        → `systemInfo.model` (real device model).
 *   - hashrate     → `status.hashrate_ghs` (fallback `heaterStatus`), shown
 *                     with `formatHashrateShort` (same helper LiveAsicVisual
 *                     uses) so units are honest (GH/TH/PH).
 *   - chip temp    → max real `status.chains[].temp_c`.
 *   - wall draw    → live wall power when present; otherwise a display/model
 *                     estimate explicitly labelled as such.
 *   - BTU/h        → `heaterStatus.btu_h` else `wattsToBtu(displayPower)`.
 *   - running %    → live power vs the system's reported max (when known)
 *                     else live-vs-idle; honest "—" when unknowable.
 *   - MiniAsicGrid → the real `<LiveAsicVisual variant="heater" compact />`
 *                     (deterministic, hashrate-driven, zero fabrication).
 *   - foot line    → real chain count / chip count / chip type.
 */
export function HeaterEnginePanel() {
  const heater = useMinerStore(s => s.heaterStatus);
  const status = useMinerStore(s => s.status);
  const stats = useMinerStore(s => s.stats);
  const systemInfo = useMinerStore(s => s.systemInfo);

  const hasConnection = status != null || heater != null || systemInfo != null;

  const displayPower = getDisplayPowerWatts(heater, stats?.power);
  const liveWallPower = getLiveDisplayWallWatts(heater, stats?.power);
  const drawPower = liveWallPower > 0 ? liveWallPower : displayPower;
  const hashrateGhs = status?.hashrate_ghs ?? heater?.hashrate_ghs ?? 0;
  const isMining = hashrateGhs > 0;

  const hr = formatHashrateShort(hashrateGhs);

  const realChains = status?.chains ?? [];
  const chainCount = realChains.length || systemInfo?.chain_count || 0;
  const chipCount = realChains.reduce((sum, c) => sum + Math.max(0, c.chips), 0)
    || systemInfo?.chip_count
    || 0;
  const maxChipTemp = realChains.reduce((m, c) => Math.max(m, c.temp_c || 0), 0);

  const btuIsLive = liveWallPower > 0;
  const btu = btuIsLive
    ? wattsToBtu(liveWallPower)
    : heater?.btu_h ?? (displayPower > 0 ? wattsToBtu(displayPower) : 0);
  const chipType = systemInfo?.chip_type ?? '';
  const model = systemInfo?.model ?? '';

  // Honest "running %": live wall power against the targeted wattage when
  // the stats power branch actually reports one; otherwise we only know
  // live-vs-idle. Never fabricated. `stats.power` is a union — `targeting`
  // only exists on the rich PowerStats branch (same `in`-narrowing guard
  // HeaterStatus.tsx uses for `watt_cap`).
  const statsPower = stats?.power;
  const maxWatts =
    statsPower && 'targeting' in statsPower
      ? statsPower.targeting?.target_watts ?? 0
      : 0;
  const runPctText =
    liveWallPower > 0 && maxWatts > 0
      ? `${Math.min(100, Math.round((liveWallPower / maxWatts) * 100))}%`
      : isMining
        ? 'hashing'
        : '—';

  const tempWarn = maxChipTemp >= 80;

  return (
    <div
      className="nest-card nest-engine"
      data-tooltip={glossaryText('btu_per_hour')}
    >
      <div className="nest-engine-eyebrow">
        <EngineGlyph />
        The engine
      </div>

      <div className="nest-engine-head">
        <div>
          <div className="nest-engine-name">
            {model || (hasConnection ? 'Miner' : '—')}
          </div>
          <div className="nest-engine-meta">
            running at <strong>{runPctText}</strong>
            {btu > 0 ? <> · {btu.toLocaleString()} {btuIsLive ? 'BTU/h' : 'BTU/h est.'}</> : null}
          </div>
        </div>
        <span className={`nest-engine-pill ${isMining ? 'on' : 'off'}`}>
          <span className="nest-engine-pill-dot" />
          {isMining ? 'Hashing' : hasConnection ? 'Idle' : 'Offline'}
        </span>
      </div>

      <div className="nest-engine-stats">
        <div
          className="nest-stat"
          data-tooltip={glossaryText('hashrate_local_vs_pool')}
        >
          <span className="nest-stat-icon">
            <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <path d="M13 2L3 14h9l-1 8 10-12h-9l1-8z" />
            </svg>
          </span>
          <span className="nest-stat-label">Hashrate</span>
          <span className="nest-stat-value">
            {isMining ? hr.value : '—'}
            <span className="nest-stat-unit">{isMining ? hr.unit : ''}</span>
          </span>
        </div>

        <div
          className="nest-stat"
          data-tooltip={glossaryText('temp_die_vs_board')}
        >
          <span
            className="nest-stat-icon"
            style={{ color: tempWarn ? 'var(--yellow)' : undefined }}
          >
            <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <path d="M14 14.76V3.5a2.5 2.5 0 0 0-5 0v11.26a4.5 4.5 0 1 0 5 0z" />
            </svg>
          </span>
          <span className="nest-stat-label">Chip temp</span>
          <span className="nest-stat-value">
            {maxChipTemp > 0 ? maxChipTemp.toFixed(0) : '—'}
            <span className="nest-stat-unit">{maxChipTemp > 0 ? '°C' : ''}</span>
          </span>
        </div>

        <div
          className="nest-stat"
          data-tooltip={glossaryText('wall_power')}
        >
          <span className="nest-stat-icon">
            <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <path d="M9.59 4.59A2 2 0 1 1 11 8H2m10.59 11.41A2 2 0 1 0 14 16H2m15.73-8.27A2.5 2.5 0 1 1 19.5 12H2" />
            </svg>
          </span>
          <span className="nest-stat-label">Draw</span>
          <span className="nest-stat-value">
            {drawPower > 0 ? (drawPower / 1000).toFixed(2) : '—'}
            <span className="nest-stat-unit">{drawPower > 0 ? (liveWallPower > 0 ? 'kW' : 'kW est.') : ''}</span>
          </span>
        </div>
      </div>

      {/* The full silicon grid lives in a dedicated full-width section below
          the hero (BasicDashboard heater-home) rather than crammed into this
          side card — that keeps the engine panel a compact stats card so the
          right column height-matches the left dial column, and gives the
          per-chain ASIC visual the width it actually needs. */}

      <div className="nest-engine-foot">
        <span className="nest-engine-foot-l">
          <span className={`nest-engine-foot-dot ${isMining ? 'on' : ''}`} />
          {isMining ? 'hashing live' : hasConnection ? 'idle' : 'offline'}
        </span>
        <span className="nest-engine-foot-r">
          {chainCount > 0 ? `${chainCount} chain${chainCount === 1 ? '' : 's'}` : '— chains'}
          {' · '}
          {chipCount > 0 ? `${chipCount.toLocaleString()} chips` : '— chips'}
          {chipType ? ` · ${chipType}` : ''}
        </span>
      </div>
    </div>
  );
}

// Kit HeaterMode.jsx `DCentralMolecule` (size 16, no glow) — the small
// tri-node D-Central mark used in the engine eyebrow.
function EngineGlyph() {
  const id = React.useId();
  return (
    <svg width="16" height="16" viewBox="0 0 64 64" aria-hidden="true" style={{ flexShrink: 0 }}>
      <defs>
        <radialGradient id={`dcm-eng-${id}`} cx="38%" cy="28%" r="70%">
          <stop offset="0%" stopColor="#FFD47A" />
          <stop offset="55%" stopColor="#FAA500" />
          <stop offset="100%" stopColor="#FA6700" />
        </radialGradient>
      </defs>
      <line x1="22" y1="26" x2="42" y2="26" stroke="#111" strokeWidth="3" strokeLinecap="round" />
      <line x1="32" y1="44" x2="22" y2="26" stroke="#111" strokeWidth="3" strokeLinecap="round" />
      <line x1="32" y1="44" x2="42" y2="26" stroke="#111" strokeWidth="3" strokeLinecap="round" />
      <circle cx="22" cy="26" r="10" fill={`url(#dcm-eng-${id})`} />
      <circle cx="42" cy="26" r="10" fill={`url(#dcm-eng-${id})`} />
      <circle cx="32" cy="44" r="10" fill={`url(#dcm-eng-${id})`} />
    </svg>
  );
}
