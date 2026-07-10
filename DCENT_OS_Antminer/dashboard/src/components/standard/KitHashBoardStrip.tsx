// KitHashBoardStrip — structural recreation of the design-kit's
// `DashboardPage.jsx` HashBoardStrip section.
//
// Kit reference: ui_kits/dashboard/DashboardPage.jsx (HashBoardStrip):
//   <div className="section">
//     <div className="section-title">Hash Boards <span className="small-tag accent">…</span>
//        <a className="right">Configure hash boards</a></div>
//     <grid 3-col>
//        <div className="chain-card">
//          <div className="chain-head"><span className="live-dot"/><span className="chain-title"/>
//             <span className="chain-connector"/><button className="icon-btn"/></div>
//          2× <div className="chain-row"> 2× {chain-label + chain-value} </div>
//          chips-health: 3× {chain-chip-num + label}
//        </div>
//     </grid>
//     <ChipMap/> when a card is clicked
//   </div>
//
// Every card is fed from REAL `status.chains` + `stats.chains`. The expanded
// per-chip map reuses production's real `ChipHeatMap` (live chain data).
// Honest empty state when no chains are detected — never fabricated cards.
import React, { useCallback, useEffect, useRef, useState, type CSSProperties } from 'react';
import { useMinerStore } from '../../store/miner';
import { ChipHeatMap } from './ChipHeatMap';
import { PerChainTelemetryStrip } from './PerChainTelemetryStrip';
import { formatHashrateShort, formatVoltage, formatFrequency } from '../../utils/format';
import { useFxPulse, useRewardFx } from '../../fx/useRewardFx';

const CONNECTOR_LABELS = ['J6', 'J7', 'J8', 'J9'];

function PowerIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <path d="M10 3v7" />
      <path d="M6 6a6 6 0 108 0" />
    </svg>
  );
}

export function KitHashBoardStrip() {
  const chains = useMinerStore(s => s.status?.chains ?? []);
  const statsChains = useMinerStore(s => s.stats?.chains ?? []);
  const chipType = useMinerStore(s => s.systemInfo?.chip_type ?? '');
  const [openMap, setOpenMap] = useState<number | null>(null);
  const [workFresh, pulseWorkFresh] = useFxPulse(320);
  const [chainFx, setChainFx] = useState<{ chainId: number; intensity: number } | null>(null);
  const chainFxTimerRef = useRef<number | null>(null);

  useEffect(() => () => {
    if (chainFxTimerRef.current !== null) {
      window.clearTimeout(chainFxTimerRef.current);
    }
  }, []);

  const pulseChain = useCallback((chainId: number, intensity: number) => {
    if (chainFxTimerRef.current !== null) {
      window.clearTimeout(chainFxTimerRef.current);
    }
    setChainFx({ chainId, intensity });
    chainFxTimerRef.current = window.setTimeout(() => {
      setChainFx(null);
      chainFxTimerRef.current = null;
    }, 520);
  }, []);

  useRewardFx(useCallback((event) => {
    if (event.intensity <= 0) return;
    if (event.kind === 'nonce-activity' && typeof event.chainId === 'number') {
      pulseChain(event.chainId, event.intensity);
    } else if (event.kind === 'work-fresh') {
      pulseWorkFresh();
    }
  }, [pulseChain, pulseWorkFresh]));

  if (chains.length === 0) {
    return (
      <div className="section" data-testid="kit-hashboard-strip">
        <div className="section-title">Hash Boards</div>
        <div
          style={{
            padding: '28px 12px',
            textAlign: 'center',
            color: 'var(--fg-dim)',
            fontSize: '.85rem',
          }}
        >
          No hashboards detected. Check ribbon cables and chain power — dcentrald
          rescans on the next tick.
        </div>
      </div>
    );
  }

  const totalHr = chains.reduce((s, c) => s + c.hashrate_ghs, 0);
  const totalHrFmt = formatHashrateShort(totalHr);
  const avgFreq = chains.length > 0
    ? Math.round(chains.reduce((s, c) => s + c.frequency_mhz, 0) / chains.length)
    : 0;
  const activeCount = chains.filter(c => c.chips > 0).length;

  return (
    <div className="section" data-testid="kit-hashboard-strip">
      <div className="section-title">
        Hash Boards
        <span className={`small-tag accent ${workFresh ? 'dcfx-work-fresh' : ''}`}>
          {chipType ? `${chipType} · ` : ''}{totalHrFmt.value} {totalHrFmt.unit}
          {avgFreq > 0 ? ` · ${avgFreq} MHz` : ''}
          {` · ${activeCount}/${chains.length} active`}
        </span>
      </div>
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(3,1fr)', gap: 12 }} className="chain-strip">
        {chains.map((chain, i) => {
          const sChain = statsChains.find(s => s.id === chain.id);
          const hwErrors = sChain?.hw_errors ?? 0;
          const isActive = (chain.status ?? '').toLowerCase() === 'active' || chain.hashrate_ghs > 0;
          const isUnpowered = chain.hashrate_ghs === 0 && chain.frequency_mhz === 0
            && chain.voltage_mv === 0 && chain.temp_c === 0;
          const hrFmt = formatHashrateShort(chain.hashrate_ghs);
          // Honest chip health: chips reported vs. errored. We do NOT
          // fabricate a healthy/unknown/unhealthy split — production only
          // knows total chips + error count, so "unknown" = total minus
          // observed-unhealthy and we never invent a "healthy" count we
          // don't have. Healthy is shown only when the chain is actively
          // hashing and error-free.
          const totalChips = chain.chips;
          const unhealthy = (chain.errors ?? 0) + hwErrors > 0 ? Math.min(totalChips, (chain.errors ?? 0) + hwErrors) : 0;
          const healthy = isActive && unhealthy === 0 ? totalChips : 0;
          const unknown = Math.max(0, totalChips - healthy - unhealthy);
          const isOpen = openMap === i;
          const chainFxActive = chainFx?.chainId === chain.id;
          const chainCardStyle = {
            cursor: 'pointer',
            ...(chainFxActive ? { '--dcfx-intensity': String(chainFx.intensity) } : {}),
          } as CSSProperties;

          return (
            <div
              key={chain.id}
              className={`chain-card dcfx-contained ${chainFxActive ? 'dcfx-chain-shimmer' : ''}`}
              onClick={() => setOpenMap(isOpen ? null : i)}
              style={chainCardStyle}
              data-tip={isOpen ? 'Hide chip map' : 'Show per-chip heatmap'}
              role="button"
              tabIndex={0}
              aria-expanded={isOpen}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault();
                  setOpenMap(isOpen ? null : i);
                }
              }}
            >
              <span className="dcfx-chain-sweep" aria-hidden="true" />
              <div className="chain-head">
                {isActive && <span className="live-dot" aria-hidden="true" />}
                <span className="chain-id chain-title">
                  Hashboard #{chain.id}
                </span>
                <span className="chain-connector">{CONNECTOR_LABELS[i] ?? `J${chain.id}`}</span>
              </div>
              {isUnpowered ? (
                <div className="cp-chain-standby">Standby — chain reports zero power</div>
              ) : (
                <>
                  <div className="chain-row">
                    <div>
                      <div className="chain-label label">Current voltage</div>
                      <div className="chain-value value">
                        {chain.voltage_mv > 0 ? formatVoltage(chain.voltage_mv) : '—'}
                      </div>
                    </div>
                    <div>
                      <div className="chain-label label">Current frequency</div>
                      <div className="chain-value value">
                        {chain.frequency_mhz > 0 ? formatFrequency(chain.frequency_mhz) : '—'}
                      </div>
                    </div>
                  </div>
                  <div className="chain-row">
                    <div>
                      <div className="chain-label label">Hashrate</div>
                      <div className="chain-value value">
                        {chain.hashrate_ghs > 0 ? `${hrFmt.value} ${hrFmt.unit}` : '—'}
                      </div>
                    </div>
                    <div>
                      <div className="chain-label label">Temperature</div>
                      <div className="chain-value value">
                        {chain.temp_c > 0 ? `${chain.temp_c.toFixed(1)} °C` : '—'}
                      </div>
                    </div>
                  </div>
                  <div
                    style={{
                      marginTop: 12,
                      paddingTop: 10,
                      borderTop: '1px solid var(--border-subtle)',
                    }}
                  >
                    <div className="chain-label label" style={{ marginBottom: 6 }}>
                      Chips health ({totalChips} chips)
                    </div>
                    <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr 1fr', gap: 4 }}>
                      <div>
                        <span className="chain-chip-num chain-value" style={{ color: 'var(--green)' }}>{healthy}</span>
                        <div style={{ fontSize: '.7rem', color: 'var(--fg-secondary, #b7b7c6)' }}>Healthy</div>
                      </div>
                      <div>
                        <span className="chain-chip-num chain-value" style={{ color: 'var(--fg-dim)' }}>{unknown}</span>
                        <div style={{ fontSize: '.7rem', color: 'var(--fg-secondary, #b7b7c6)' }}>Unknown</div>
                      </div>
                      <div>
                        <span
                          className="chain-chip-num chain-value"
                          style={{ color: unhealthy > 0 ? 'var(--red)' : 'var(--fg-dim)' }}
                        >
                          {unhealthy}
                        </span>
                        <div style={{ fontSize: '.7rem', color: 'var(--fg-secondary, #b7b7c6)' }}>Unhealthy</div>
                      </div>
                    </div>
                  </div>
                </>
              )}
            </div>
          );
        })}
      </div>
      {openMap !== null && chains[openMap] && (
        <div id={`chip-heatmap-${chains[openMap].id}`} style={{ marginTop: 12 }}>
          <ChipHeatMap chainIndex={openMap} chainId={chains[openMap].id} />
        </div>
      )}
      {/* Per-chain voltage/frequency detail, folded into this same section so
          Hash Boards isn't split across two repetitive cards (merged 2026-06-25). */}
      <div style={{ marginTop: 14, paddingTop: 12, borderTop: '1px solid var(--border-subtle)' }}>
        <div className="chain-label label" style={{ marginBottom: 8 }}>
          Per-Chain Detail <span className="small-tag muted">voltage · frequency · live</span>
        </div>
        <PerChainTelemetryStrip />
      </div>
    </div>
  );
}
