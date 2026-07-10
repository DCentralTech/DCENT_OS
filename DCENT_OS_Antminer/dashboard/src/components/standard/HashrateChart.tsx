import React, { useState, useMemo, useCallback, useEffect, useRef } from 'react';
import { useMinerStore } from '../../store/miner';
import { api } from '../../api/client';
import type { RollingMetricsResponse } from '../../api/types';
import { TIME_RANGES } from '../../utils/constants';
import { SvgChart, ChartSeries } from '../common/SvgChart';
import type { StatusState } from '../common/StatusPill';
import { classifyMiningState } from '../../utils/statusStates';

/**
 * COMP-HERO contract surface (DCENT Design Language —
 * component-contract.md §2). The OS HashrateHero is emitted as the page-hero
 * COMPOSITION — the chart (`HashrateChart`, here) + the KPI rail
 * (`KitOverviewChart` + `KitStatsKpiGrid`/`KpiCard`) assembled at the page
 * level. This `HashrateHeroProps` type ADVERTISES the exact §2 contract surface
 * so the OS hero is contract-legible against axe's `mining-core` emission; it is
 * a pure props/type advertisement — the hero render stays the chart + KPI-rail
 * composition (no render rewrite here).
 *
 * §2 contract props (substrate-neutral):
 *   { hashrate, unit, expected?, efficiencyJth?, sharesAccepted, bestDiff,
 *     miningState }
 *
 * §2 derived-field rules (BOTH sides already do this — pinned here):
 *   unit auto-scale: >=1e6 GH ⇒ PH/s · >=1000 ⇒ TH/s · >=1 ⇒ GH/s · else MH/s
 *   efficiency:      hashrate>0 && watts>0 ? watts/(hashrate/1000) : null  (null ⇒ '--')
 *   capacityPct:     expected>0 ? min(150, round(hashrate/expected*100)) : null
 *
 * HONEST-NULL contract: `bestDiff` and `expected` are declared
 * nullable/optional with honest-null defaults because no first-class wire field
 * exists for them yet (api/types.ts:1408 marks `bestDiff` "NOT real" — an
 * AxeOS/pyasic-compatibility field the daemon emits as 0 only for ecosystem
 * compatibility, NEVER measured telemetry). The contract is advertised without
 * implying fabricated data; a real wire field is NOT invented here.
 *
 * KEEP-UNIQUE (§2): the OS render is the chart + command-core KPI rail
 * (industrial-operator identity); axe's molecular sphere is `[axe-only]`.
 */
export type HrUnit = 'MH/s' | 'GH/s' | 'TH/s' | 'PH/s';

export interface HashrateHeroProps {
  /** Internal GH/s (units per terminology TERM-3). */
  hashrate: number;
  /** Auto-scaled display unit (per the §2 ladder). */
  unit: HrUnit;
  /** Expected/nominal GH/s for the "% capacity" sub. Honest-null when absent. */
  expected?: number | null;
  /** J/TH, "lower is better"; null ⇒ "--" (never a fabricated 0). */
  efficiencyJth?: number | null;
  sharesAccepted: number;
  /**
   * Session best difficulty. `number | null` with an honest-null default —
   * there is no first-class `bestDiff` wire field (types.ts:1408 "NOT real");
   * null renders the canonical empty glyph, never a fabricated 0.
   */
  bestDiff: number | null;
  /** Reuses the COMP-PILL `StatusState` enum (the hero embeds a StatusPill). */
  miningState: StatusState;
}

/**
 * Auto-scale a GH/s value onto the §2 display-unit ladder. Pure helper that
 * pins the contract's unit ladder (>=1e6 GH ⇒ PH/s · >=1000 ⇒ TH/s · >=1 ⇒
 * GH/s · else MH/s) so the advertised `unit` is derived, never guessed.
 */
export function heroHrUnit(hashrateGhs: number): HrUnit {
  if (hashrateGhs >= 1e6) return 'PH/s';
  if (hashrateGhs >= 1000) return 'TH/s';
  if (hashrateGhs >= 1) return 'GH/s';
  return 'MH/s';
}

/**
 * Derive the hero's canonical `miningState` (a COMP-PILL `StatusState`) from the
 * mining-enabled flag + a positive-hashrate observation, REUSING the canonical
 * `classifyMiningState` helper (utils/statusStates.ts). This guarantees the
 * hero pill is the canonical truth-ladder state (`mining` / `ready` / `standby`)
 * — never a fabricated label. Pre-first-data callers pass `telemetry_pending`
 * directly (FIRST PAINT must be that, never `mining`).
 */
export function heroMiningState(
  miningEnabled: boolean,
  hasPositiveHashrate: boolean,
): StatusState {
  return classifyMiningState(miningEnabled, hasPositiveHashrate);
}

// Time ranges shown in the picker. The chart owns its own ladder; the final
// range is local session history until backend long-range history is hydrated.
const CHART_TIME_RANGES: Array<{ label: string; seconds: number }> = [
  { label: '5m', seconds: 300 },
  { label: '1h', seconds: 3600 },
  { label: '24h', seconds: 86400 },
  { label: 'Session', seconds: 604800 },
];

interface SeriesToggleState {
  hashrate: boolean;
  temp: boolean;
  power: boolean;
  shares: boolean;
}

interface HashrateChartProps {
  compact?: boolean;
}

const DEFAULT_TOGGLES: SeriesToggleState = {
  hashrate: true,
  temp: false,
  power: false,
  shares: false,
};

const HASHRATE_COLOR = 'var(--accent, #FAA500)';
const TEMP_COLOR = 'var(--red, #FF6B6B)';
const POWER_COLOR = 'var(--accent-light, #6CB4FF)';
const SHARES_COLOR = 'var(--green, #2DD4A0)';
const ROLLING_METRICS_POLL_MS = 10_000;
const MAX_ROLLING_POINTS = 720;

type ChartPoint = { time: number; value: number };

export function rollingHashratePoint(metrics: RollingMetricsResponse): ChartPoint | null {
  if (metrics.w1m.sample_count <= 0) return null;
  const avgThs = metrics.w1m.avg_hashrate_ths;
  if (!Number.isFinite(avgThs) || avgThs < 0) return null;
  const at = Number.isFinite(metrics.now_ms) && metrics.now_ms > 0
    ? metrics.now_ms / 1000
    : Date.now() / 1000;
  return { time: at, value: avgThs * 1000 };
}

export function HashrateChart({ compact = false }: HashrateChartProps = {}) {
  // Default to 1h — the most useful at-a-glance window.
  const [timeRange, setTimeRange] = useState<number>(1);
  const [toggles, setToggles] = useState<SeriesToggleState>(DEFAULT_TOGGLES);
  const [rollingHashrateHistory, setRollingHashrateHistory] = useState<ChartPoint[]>([]);
  const timeRangeLabel = 'Hashrate chart time range';

  const hashrateHistory = useMinerStore(s => s.hashrateHistory);
  const tempHistory = useMinerStore(s => s.tempHistory);
  const powerHistory = useMinerStore(s => s.powerHistory);
  const accepted = useMinerStore(s => s.status?.accepted ?? 0);

  useEffect(() => {
    let cancelled = false;

    const pollRollingMetrics = async () => {
      try {
        const metrics = await api.getRollingMetrics();
        if (cancelled || !metrics) return;
        const point = rollingHashratePoint(metrics);
        if (!point) return;
        setRollingHashrateHistory(prev => {
          const last = prev[prev.length - 1];
          if (last && last.time === point.time) {
            return [...prev.slice(0, -1), point];
          }
          const next = [...prev, point];
          return next.length > MAX_ROLLING_POINTS ? next.slice(-MAX_ROLLING_POINTS) : next;
        });
      } catch {
        // Optional daemon route. Existing session history remains the fallback.
      }
    };

    pollRollingMetrics();
    const id = setInterval(pollRollingMetrics, ROLLING_METRICS_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, []);

  // Local in-component history for accepted shares (the store doesn't track
  // accepted-share delta). Snapshot the running total alongside each new
  // hashrate sample and emit a delta-per-minute series.
  type SharesPoint = { time: number; value: number; _accepted: number };
  const sharesHistoryRef = useRef<SharesPoint[]>([]);
  const lastHashrateLenRef = useRef(0);
  useEffect(() => {
    if (hashrateHistory.length === lastHashrateLenRef.current) return;
    lastHashrateLenRef.current = hashrateHistory.length;
    const lastPoint = hashrateHistory[hashrateHistory.length - 1];
    if (!lastPoint) return;
    const prev = sharesHistoryRef.current[sharesHistoryRef.current.length - 1];
    let rate = 0;
    if (prev) {
      const dt = Math.max(1, lastPoint.time - prev.time);
      const dShares = Math.max(0, accepted - prev._accepted);
      rate = (dShares / dt) * 60; // shares per minute
    }
    const next = [...sharesHistoryRef.current,
      { time: lastPoint.time, value: rate, _accepted: accepted }];
    sharesHistoryRef.current = next.length > 1440 ? next.slice(-1440) : next;
  }, [hashrateHistory, accepted]);

  // Responsive chart height — kept compact so the plot hugs its data
  // instead of floating in a tall empty band.
  const getChartHeight = useCallback(() => {
    if (compact) return 148;
    const w = typeof window !== 'undefined' ? window.innerWidth : 1024;
    if (w < 500) return 160;
    if (w < 800) return 180;
    return 200;
  }, [compact]);

  // Availability detection — used to gracefully hide series toggles.
  const hasTempData = useMemo(() => tempHistory.some(p => p.value > 0), [tempHistory]);
  const hasPowerData = useMemo(() => powerHistory.some(p => p.value > 0), [powerHistory]);
  const hasShareData = useMemo(
    () => accepted > 0 || sharesHistoryRef.current.some(p => p.value > 0),
    [accepted, hashrateHistory.length]
  );

  const rangeSeconds = CHART_TIME_RANGES[timeRange].seconds;
  const cutoff = Date.now() / 1000 - rangeSeconds;
  const hasRollingMetricForWindow = rollingHashrateHistory.some(p => p.time >= cutoff);
  const hashrateLegendLabel = hasRollingMetricForWindow
    ? 'Hashrate 1m avg (TH/s)'
    : 'Hashrate (TH/s)';
  const hashrateSourceLabel = hasRollingMetricForWindow
    ? '1m avg from daemon rolling metrics'
    : 'session samples';

  // Filter data by time range and build series
  const series = useMemo(() => {
    const result: ChartSeries[] = [];

    if (toggles.hashrate) {
      const rollingFiltered = rollingHashrateHistory.filter(p => p.time >= cutoff);
      const filtered = rollingFiltered.length > 0
        ? rollingFiltered
        : hashrateHistory.filter(p => p.time >= cutoff);
      result.push({
        data: filtered,
        color: HASHRATE_COLOR,
        label: rollingFiltered.length > 0 ? 'Hashrate 1m avg (TH/s)' : 'Hashrate (TH/s)',
        yAxis: 'left',
      });
    }

    if (toggles.temp && hasTempData) {
      const filtered = tempHistory.filter(p => p.time >= cutoff && p.value > 0);
      if (filtered.length > 0) {
        result.push({
          data: filtered,
          color: TEMP_COLOR,
          label: 'Temp (°C)',
          dashed: true,
          yAxis: 'right',
        });
      }
    }

    if (toggles.power && hasPowerData) {
      const filtered = powerHistory.filter(p => p.time >= cutoff && p.value > 0);
      if (filtered.length > 0) {
        result.push({
          data: filtered,
          color: POWER_COLOR,
          label: 'Power (W)',
          dashed: true,
          yAxis: 'right',
        });
      }
    }

    if (toggles.shares && hasShareData) {
      const filtered = sharesHistoryRef.current.filter(p => p.time >= cutoff);
      if (filtered.length > 0) {
        result.push({
          data: filtered,
          color: SHARES_COLOR,
          label: 'Shares/min',
          dashed: true,
          yAxis: 'right',
        });
      }
    }

    return result;
  }, [hashrateHistory, rollingHashrateHistory, tempHistory, powerHistory, cutoff, toggles,
      hasTempData, hasPowerData, hasShareData]);

  const formatValue = useCallback((value: number, seriesIndex: number) => {
    // Left axis (index 0) = hashrate GH/s shown as TH/s
    if (seriesIndex === 0 && toggles.hashrate) {
      return (value / 1000).toFixed(2);
    }
    return value.toFixed(1);
  }, [toggles.hashrate]);

  const toggle = useCallback(
    (key: keyof SeriesToggleState) =>
      setToggles(prev => ({ ...prev, [key]: !prev[key] })),
    []
  );

  // Visible-series count for legend caption
  const visibleCount = (toggles.hashrate ? 1 : 0)
    + (toggles.temp && hasTempData ? 1 : 0)
    + (toggles.power && hasPowerData ? 1 : 0)
    + (toggles.shares && hasShareData ? 1 : 0);

  // SR summary: announce hashrate window + avg in TH/s. Computed from the
  // exact series the chart will render so SR truth tracks visible truth.
  const summaryText = useMemo(() => {
    const rangeLabel = CHART_TIME_RANGES[timeRange].label;
    const hashrateSeries = series.find(s => s.label?.startsWith('Hashrate'));
    if (!hashrateSeries || hashrateSeries.data.length === 0) {
      return `Hashrate chart, ${rangeLabel} window, no data yet`;
    }
    let sum = 0;
    let count = 0;
    for (const p of hashrateSeries.data) {
      if (Number.isFinite(p.value)) { sum += p.value; count++; }
    }
    if (count === 0) return `Hashrate chart, ${rangeLabel} window, no data yet`;
    const avgThs = (sum / count) / 1000; // GH/s -> TH/s
    return `Hashrate over last ${rangeLabel}: ${avgThs.toFixed(2)} TH/s average across ${count} samples`;
  }, [series, timeRange]);

  return (
    <div className={`chart-container chart-wrap ds-hashrate-chart${compact ? ' ds-hashrate-chart-compact' : ''}`}>
      {/* Parent sections provide the chart title; this row carries only the time-range picker. */}
      <div className="chart-header hashrate-chart-controls hashrate-chart-controls-tabs-only">
        <div className="tab-underline time-range-tabs" role="group" aria-label={timeRangeLabel}>
          {CHART_TIME_RANGES.map((range, i) => (
            <button
              key={range.label}
              type="button"
              className={`time-tab ${timeRange === i ? 'active' : ''}`}
              onClick={() => setTimeRange(i)}
              aria-pressed={timeRange === i}
              aria-label={`Show ${range.label} hashrate history`}
            >
              {range.label}
            </button>
          ))}
        </div>
      </div>

      {/* (Removed the hidden width:0 <svg><defs> gradient block — it was dead
          code. SvgChart generates its own per-series area-fill gradients with
          useId(); nothing ever referenced these #hashrate-fill/#temp-fill/etc
          gradient IDs.) */}

      <div className="ds-hashrate-chart-body">
        <SvgChart
          series={series}
          height={getChartHeight()}
          formatValue={formatValue}
          summaryText={summaryText}
        />
      </div>

      <fieldset className="chart-legend legend" style={{ border: 0, padding: 0, margin: 0 }}>
        <legend className="sr-only">Visible chart series</legend>

        <label
          className={`chart-legend-toggle ${toggles.hashrate ? '' : 'inactive'}`}
          style={{ color: HASHRATE_COLOR }}
        >
          <input
            type="checkbox"
            checked={toggles.hashrate}
            onChange={() => toggle('hashrate')}
            aria-label="Toggle hashrate series"
          />
          <span
            className="swatch chart-legend-swatch"
            style={{ background: HASHRATE_COLOR }}
            aria-hidden="true"
          />
          {hashrateLegendLabel}
        </label>

        {hasTempData && (
          <label
            className={`chart-legend-toggle ${toggles.temp ? '' : 'inactive'}`}
            style={{ color: TEMP_COLOR }}
          >
            <input
              type="checkbox"
              checked={toggles.temp}
              onChange={() => toggle('temp')}
              aria-label="Toggle temperature series"
            />
            <span
              className="swatch chart-legend-swatch dashed"
              style={{ background: TEMP_COLOR }}
              aria-hidden="true"
            />
            Temp ({'°'}C)
          </label>
        )}

        {hasPowerData && (
          <label
            className={`chart-legend-toggle ${toggles.power ? '' : 'inactive'}`}
            style={{ color: POWER_COLOR }}
          >
            <input
              type="checkbox"
              checked={toggles.power}
              onChange={() => toggle('power')}
              aria-label="Toggle power series"
            />
            <span
              className="swatch chart-legend-swatch dashed"
              style={{ background: POWER_COLOR }}
              aria-hidden="true"
            />
            Power (W)
          </label>
        )}

        {hasShareData && (
          <label
            className={`chart-legend-toggle ${toggles.shares ? '' : 'inactive'}`}
            style={{ color: SHARES_COLOR }}
          >
            <input
              type="checkbox"
              checked={toggles.shares}
              onChange={() => toggle('shares')}
              aria-label="Toggle shares series"
            />
            <span
              className="swatch chart-legend-swatch dashed"
              style={{ background: SHARES_COLOR }}
              aria-hidden="true"
            />
            Shares/min
          </label>
        )}

        <span className="chart-legend-note" aria-live="polite">
          {visibleCount} series visible {'·'} {hashrateSourceLabel}
        </span>
      </fieldset>
    </div>
  );
}

// Keep the original TIME_RANGES import alive (it remains the canonical
// dashboard-wide range catalog; the chart owns its own short ladder).
void TIME_RANGES;
