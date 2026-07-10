import React, { useEffect, useMemo, useRef } from 'react';
import { useMinerStore } from '../../store/miner';
import { Sparkline } from '../common/Sparkline';
import type { ChainState } from '../../api/types';
import { formatHashrateShort, formatVoltage, formatFrequency } from '../../utils/format';
import { Tooltip } from '../common/Tooltip';
import { glossaryText } from '../../utils/glossary';
import { boardName, boardDescriptor } from '../../utils/boardLabel';

const PER_CHAIN_SPARK_LEN = 40; // ~5min worth of points at 1/7.5s polling

interface PerChainHistoryEntry {
  hashrate: number[];
  temp: number[];
}

function tempTone(t: number): { color: string; label: string } {
  if (t <= 0) return { color: 'var(--text-dim)', label: 'idle' };
  if (t < 55) return { color: 'var(--green)', label: 'cool' };
  if (t < 65) return { color: 'var(--green)', label: 'nominal' };
  if (t < 72) return { color: 'var(--yellow)', label: 'warm' };
  if (t < 80) return { color: 'var(--yellow)', label: 'hot' };
  return { color: 'var(--red)', label: 'critical' };
}

// Map temperature 0..100 C to mini-bar fraction (clamped)
function tempBarFraction(t: number): number {
  if (t <= 0) return 0;
  return Math.max(0, Math.min(1, t / 100));
}

function rangePercent(value: number, min: number, max: number): number {
  if (max <= min) return 0;
  return Math.max(0, Math.min(100, ((value - min) / (max - min)) * 100));
}

interface PerChainTelemetryStripProps {
  /** Override chain data; defaults to useMinerStore.status?.chains */
  chains?: ChainState[];
  /** Power-cap budget for power% bar */
  perChainBudgetWatts?: number | null;
  /** Optional per-chain wall-watts (from stats.power.per_chain_w) */
  perChainWatts?: number[];
}

export function PerChainTelemetryStrip({
  chains: chainsOverride,
  perChainBudgetWatts,
  perChainWatts,
}: PerChainTelemetryStripProps) {
  const storeChains = useMinerStore(s => s.status?.chains ?? []);
  const chains = chainsOverride ?? storeChains;

  // Local ring buffer for per-chain history (sparkline)
  const historyRef = useRef<Map<number, PerChainHistoryEntry>>(new Map());
  const tickRef = useRef(0);

  // Push each chain's current sample on every render where chains changed
  // (Status updates trigger re-render via the store.)
  useEffect(() => {
    if (chains.length === 0) return;
    const map = historyRef.current;
    for (const c of chains) {
      const ths = c.hashrate_ghs / 1000;
      let entry = map.get(c.id);
      if (!entry) {
        entry = { hashrate: [], temp: [] };
        map.set(c.id, entry);
      }
      entry.hashrate = [...entry.hashrate, ths].slice(-PER_CHAIN_SPARK_LEN);
      entry.temp = [...entry.temp, c.temp_c].slice(-PER_CHAIN_SPARK_LEN);
    }
    tickRef.current += 1;
  }, [chains]);

  const items = useMemo(() => chains.map((chain, i) => {
    const entry = historyRef.current.get(chain.id);
    const sparkHashrate = entry?.hashrate ?? [];
    const expectedChips = chain.chips > 0 ? chain.chips : 63;
    const hr = formatHashrateShort(chain.hashrate_ghs);
    const tone = tempTone(chain.temp_c);
    const tempFrac = tempBarFraction(chain.temp_c);
    // BUG-11: power presence proven by hashrate/freq/voltage, NOT temperature
    // (S9 board sensors are silent; a live S9 reports the die-temp fallback).
    const isUnpowered = chain.hashrate_ghs === 0 && chain.frequency_mhz === 0
      && chain.voltage_mv === 0;
    const isDieFallback = chain.temp_source === 'soc_die_fallback';
    const isActive = chain.status === 'active';
    const dotColor = isActive ? 'var(--green)'
      : chain.status === 'error' ? 'var(--red)'
      : isUnpowered ? 'var(--text-dim)'
      : 'var(--yellow)';
    const wallW = perChainWatts && perChainWatts[i] != null
      ? perChainWatts[i] : null;
    const powerPct = wallW != null && perChainBudgetWatts && perChainBudgetWatts > 0
      ? rangePercent(wallW, 0, perChainBudgetWatts) : null;
    const errs = chain.errors ?? 0;
    return {
      key: chain.id,
      boardName: boardName(i),
      boardDescriptor: boardDescriptor(i, chain.id),
      chainId: chain.id,
      hr,
      isUnpowered,
      sparkHashrate,
      tone,
      tempFrac,
      tempC: chain.temp_c,
      isDieFallback,
      dotColor,
      expectedChips,
      actualChips: chain.chips,
      voltageMv: chain.voltage_mv,
      frequencyMhz: chain.frequency_mhz,
      wallW,
      powerPct,
      errs,
    };
  }), [chains, perChainWatts, perChainBudgetWatts, tickRef.current]);

  if (chains.length === 0) {
    return null;
  }

  // P3-35(b): when 2+ chains report the SAME SoC die-temp fallback, three
  // identical pills wrongly imply three independent sensors. Surface one honest
  // shared-source caption so the operator knows it's a single die reading
  // broadcast to every chain (board sensors silent — normal on S9).
  const dieFallbackCount = items.filter(it => it.isDieFallback).length;

  return (
    <>
    <div className="per-chain-strip" data-testid="per-chain-telemetry-strip">
      {items.map(item => (
        <div
          key={item.key}
          className={`per-chain-strip-item${item.isUnpowered ? ' unpowered' : ''}`}
          data-testid={`per-chain-strip-${item.chainId}`}
        >
          {/* Header row: connector + chain id */}
          <div className="per-chain-strip-head">
            <span
              className="per-chain-strip-dot"
              aria-hidden="true"
              style={{ background: item.dotColor, boxShadow: `0 0 6px ${item.dotColor}` }}
            />
            <span className="per-chain-strip-label">{item.boardName}</span>
            <span
              className="per-chain-strip-id"
              title={`Hardware chain id ${item.chainId}`}
            >
              {item.boardDescriptor}
            </span>
            {item.errs > 0 && (
              <Tooltip
                content={`${item.errs.toLocaleString()} hardware errors on this chain. A few are normal; a sharply rising count is the signal worth checking — judge health by accepted shares, not raw HW errors.`}
              >
                <span className="per-chain-strip-err-badge">
                  {item.errs > 999 ? `${(item.errs / 1000).toFixed(1)}k` : item.errs} ERR
                </span>
              </Tooltip>
            )}
          </div>

          {/* Hashrate big value + sparkline */}
          <div className="per-chain-strip-hashrate">
            <div className="per-chain-strip-hr-value">
              {item.isUnpowered ? (
                <span className="per-chain-strip-standby">Standby</span>
              ) : (
                <>
                  <span className="per-chain-strip-hr-num">{item.hr.value}</span>
                  <span className="per-chain-strip-hr-unit">{item.hr.unit}</span>
                </>
              )}
            </div>
            <div className="per-chain-strip-spark">
              {item.sparkHashrate.length >= 2 ? (
                <Sparkline
                  data={item.sparkHashrate}
                  width={88}
                  height={26}
                  color="var(--accent)"
                />
              ) : (
                <span className="per-chain-strip-spark-empty" aria-hidden="true">{'───'}</span>
              )}
            </div>
          </div>

          {/* Temp pill */}
          <div className="per-chain-strip-temp">
            <Tooltip
              content={`${item.isDieFallback ? 'SoC die temp' : 'Temperature'} ${item.tempC.toFixed(1)}°C — ${item.tone.label}.${item.isDieFallback ? ' Board sensors returned no data (normal on S9) — this is the Zynq SoC die-temp fallback, cooler than a true board sensor.' : ''} ${glossaryText('temp_die_vs_board')}`}
            >
              <div
                className="chain-temp-pill"
                style={{ color: item.tone.color, borderColor: item.tone.color }}
              >
                <span className="chain-temp-pill-value">
                  {item.tempC > 0 ? `${item.tempC.toFixed(0)}°C${item.isDieFallback ? ' (die)' : ''}` : '--'}
                </span>
                <span className="chain-temp-pill-label">
                  {item.isDieFallback ? 'die temp' : item.tone.label}
                </span>
              </div>
            </Tooltip>
            <div className="chain-temp-bar" aria-hidden="true">
              <div
                className="chain-temp-bar-fill"
                style={{
                  width: `${item.tempFrac * 100}%`,
                  background: item.tone.color,
                }}
              />
            </div>
          </div>

          {/* Chip count */}
          <div className="per-chain-strip-stat">
            <span className="per-chain-strip-stat-label">CHIPS</span>
            <span className="per-chain-strip-stat-value">
              {item.actualChips > 0 ? `${item.actualChips}/${item.expectedChips}` : '--'}
            </span>
          </div>

          {/* Frequency */}
          <div className="per-chain-strip-stat">
            <span className="per-chain-strip-stat-label">FREQ</span>
            <span className="per-chain-strip-stat-value">
              {item.frequencyMhz > 0 ? formatFrequency(item.frequencyMhz) : '--'}
            </span>
          </div>

          {/* Voltage */}
          <div className="per-chain-strip-stat">
            <span className="per-chain-strip-stat-label">VOLT</span>
            <span className="per-chain-strip-stat-value">
              {item.voltageMv > 0 ? formatVoltage(item.voltageMv) : '--'}
            </span>
          </div>

          {/* Power % bar (if available) */}
          {item.powerPct != null && item.wallW != null ? (
            <div className="per-chain-strip-power">
              <div className="per-chain-strip-stat-label">PWR</div>
              <div className="per-chain-strip-power-bar" aria-hidden="true">
                <div
                  className="per-chain-strip-power-fill"
                  style={{
                    width: `${item.powerPct}%`,
                    background: item.powerPct > 90 ? 'var(--red)'
                      : item.powerPct > 70 ? 'var(--yellow)' : 'var(--green)',
                  }}
                />
              </div>
              <span className="per-chain-strip-stat-value">
                {item.wallW.toFixed(0)}W ({item.powerPct.toFixed(0)}%)
              </span>
            </div>
          ) : null}
        </div>
      ))}
    </div>
    {dieFallbackCount >= 2 && (
      <p
        className="per-chain-strip-die-note"
        data-testid="per-chain-die-note"
        data-tooltip={glossaryText('temp_die_vs_board')}
        style={{
          margin: '6px 2px 0',
          fontSize: '0.72rem',
          lineHeight: 1.4,
          color: 'var(--text-dim)',
        }}
      >
        Per-chain temps are one shared SoC die-temp reading broadcast to every
        chain — board sensors are silent (normal on S9), so these are not
        independent per-chain sensors.
      </p>
    )}
    </>
  );
}
