// EarningsChart — Projected sats at the current rate over time (FE-1: this is
// a PROJECTION computed from current hashrate × the live sats/day estimate, NOT
// a record of realized on-chain payouts — so the series is never labelled
// "earned"). Pure SVG line+area chart with
// orange gradient fill matching the DCENT brand palette. Time-range tabs
// (24h / 7d / 30d) drive a `period` selector; data is caller-provided
// (or pulled from a parent store). Graceful empty state when the series
// is empty. No external dependencies.
//
// Hover tooltip: timestamp + sats value, anchored to the nearest sample.
// X-axis: 3-4 tick labels at sensible intervals. Y-axis: tabular-nums.

import React, { useMemo, useRef, useState, useCallback, useEffect } from 'react';
import { isRtcSyncedMs } from '../../utils/format';

export type EarningsPeriod = '24h' | '7d' | '30d';

export interface EarningsPoint {
  ts: number; // ms epoch
  sats: number;
}

interface Props {
  period: EarningsPeriod;
  data: EarningsPoint[];
  /** Optional callback so the parent can flip the period state. */
  onPeriodChange?: (p: EarningsPeriod) => void;
}

const PERIODS: EarningsPeriod[] = ['24h', '7d', '30d'];

const W = 720;
const H = 220;
const PAD_L = 56;
const PAD_R = 16;
const PAD_T = 16;
const PAD_B = 28;

function fmtSats(v: number): string {
  return Math.round(v).toLocaleString();
}

function fmtTs(ts: number, period: EarningsPeriod): string {
  // Pre-2020 epoch = the sample was taken before the clock was synced (no RTC).
  // Render a dash instead of a misleading 1970 axis/tooltip label.
  if (!isRtcSyncedMs(ts)) return '—';
  const d = new Date(ts);
  if (period === '24h') {
    return `${d.getHours().toString().padStart(2, '0')}:${d.getMinutes().toString().padStart(2, '0')}`;
  }
  return `${d.getMonth() + 1}/${d.getDate()}`;
}

// Catmull–Rom → Cubic Bezier to render a smooth curve through the points.
// Control points are clamped to the [yMin, yMax] plot band so a spiky cumulative
// series can't overshoot the real data envelope (matches the 3 sibling charts —
// NetworkHashrateChart/Sparkline/SvgChart all clamp; this one used not to).
function smoothPath(
  pts: Array<{ x: number; y: number }>,
  yMin: number,
  yMax: number,
): string {
  if (pts.length === 0) return '';
  if (pts.length === 1) return `M ${pts[0].x} ${pts[0].y}`;
  let d = `M ${pts[0].x} ${pts[0].y}`;
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[i - 1] ?? pts[i];
    const p1 = pts[i];
    const p2 = pts[i + 1];
    const p3 = pts[i + 2] ?? p2;
    const cp1x = p1.x + (p2.x - p0.x) / 6;
    let cp1y = p1.y + (p2.y - p0.y) / 6;
    const cp2x = p2.x - (p3.x - p1.x) / 6;
    let cp2y = p2.y - (p3.y - p1.y) / 6;
    if (cp1y < yMin) cp1y = yMin;
    if (cp1y > yMax) cp1y = yMax;
    if (cp2y < yMin) cp2y = yMin;
    if (cp2y > yMax) cp2y = yMax;
    d += ` C ${cp1x} ${cp1y}, ${cp2x} ${cp2y}, ${p2.x} ${p2.y}`;
  }
  return d;
}

export function EarningsChart({ period, data, onPeriodChange }: Props) {
  const [activePeriod, setActivePeriod] = useState<EarningsPeriod>(period);
  const effective: EarningsPeriod = onPeriodChange ? period : activePeriod;
  const svgRef = useRef<SVGSVGElement | null>(null);
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);

  // Uncontrolled mode (no onPeriodChange): keep the internal selection in
  // sync if the parent changes the `period` prop. Without this the local
  // `activePeriod` goes stale and the tab highlight stops tracking the prop.
  useEffect(() => {
    if (!onPeriodChange) setActivePeriod(period);
  }, [period, onPeriodChange]);

  const setPeriod = (p: EarningsPeriod) => {
    setActivePeriod(p);
    onPeriodChange?.(p);
  };

  const { pts, minSats, maxSats, minTs, maxTs, xTicks, yTicks } = useMemo(() => {
    if (data.length === 0) {
      return {
        pts: [] as Array<{ x: number; y: number }>,
        minSats: 0, maxSats: 0, minTs: 0, maxTs: 0,
        xTicks: [] as number[], yTicks: [] as number[],
      };
    }
    const sorted = [...data].sort((a, b) => a.ts - b.ts);
    const _minTs = sorted[0].ts;
    const _maxTs = sorted[sorted.length - 1].ts;
    const tRange = Math.max(1, _maxTs - _minTs);
    let _minSats = Math.min(...sorted.map(p => p.sats));
    let _maxSats = Math.max(...sorted.map(p => p.sats));
    if (_minSats === _maxSats) {
      _maxSats = _minSats + 1;
    }
    // Band-fit the Y axis to the data instead of forcing a zero baseline.
    // Cumulative sats are always ≥ 0, so anchoring at 0 flattened the whole
    // curve into the top sliver of the plot. Add ~8% headroom below the min
    // (clamped at 0 — we never draw a negative-sats axis) so the variation
    // is actually legible. The area fill still closes to the plot floor.
    const _span = Math.max(1, _maxSats - _minSats);
    _minSats = Math.max(0, _minSats - _span * 0.08);
    const sRange = Math.max(1, _maxSats - _minSats);
    const innerW = W - PAD_L - PAD_R;
    const innerH = H - PAD_T - PAD_B;
    const _pts = sorted.map(p => ({
      x: PAD_L + ((p.ts - _minTs) / tRange) * innerW,
      y: PAD_T + innerH - ((p.sats - _minSats) / sRange) * innerH,
    }));
    // X tick anchors — 4 evenly spaced sample indices.
    const tickCount = Math.min(4, sorted.length);
    const _xTicks: number[] = [];
    if (tickCount > 1) {
      for (let i = 0; i < tickCount; i++) {
        const idx = Math.round(((sorted.length - 1) * i) / (tickCount - 1));
        _xTicks.push(idx);
      }
    } else if (tickCount === 1) {
      _xTicks.push(0);
    }
    // Y ticks — 4 lines at quartiles.
    const _yTicks: number[] = [];
    for (let i = 0; i <= 3; i++) {
      _yTicks.push(_minSats + (sRange * i) / 3);
    }
    return {
      pts: _pts,
      minSats: _minSats, maxSats: _maxSats,
      minTs: _minTs, maxTs: _maxTs,
      xTicks: _xTicks, yTicks: _yTicks,
    };
  }, [data]);

  const path = pts.length > 0 ? smoothPath(pts, PAD_T, H - PAD_B) : '';
  const areaPath = pts.length > 0
    ? `${path} L ${pts[pts.length - 1].x} ${H - PAD_B} L ${pts[0].x} ${H - PAD_B} Z`
    : '';

  const onMove = useCallback((ev: React.MouseEvent<SVGSVGElement>) => {
    if (!svgRef.current || pts.length === 0) return;
    const rect = svgRef.current.getBoundingClientRect();
    const xCoord = ((ev.clientX - rect.left) / rect.width) * W;
    // Nearest point search.
    let best = 0;
    let bestDx = Math.abs(pts[0].x - xCoord);
    for (let i = 1; i < pts.length; i++) {
      const dx = Math.abs(pts[i].x - xCoord);
      if (dx < bestDx) { best = i; bestDx = dx; }
    }
    setHoverIdx(best);
  }, [pts]);

  const onLeave = useCallback(() => setHoverIdx(null), []);

  const sorted = useMemo(
    () => (data.length > 0 ? [...data].sort((a, b) => a.ts - b.ts) : []),
    [data],
  );

  const isEmpty = data.length === 0;

  // STALE cue: the newest sample is older than 10 minutes. Earnings samples
  // are derived from hashrate history, so a stale chart means telemetry
  // stopped flowing — the last points are real, never faked/extrapolated.
  // (TRUTH CONTRACT: this only annotates; it never hides or softens data.)
  const STALE_AFTER_MS = 10 * 60 * 1000;
  const isStale = !isEmpty && sorted.length > 0
    && Date.now() - sorted[sorted.length - 1].ts > STALE_AFTER_MS;

  return (
    <div className="earnings-chart" data-testid="earnings-chart">
      <div className="tab-underline time-range-tabs" role="tablist" aria-label="Earnings time range">
        {PERIODS.map(p => (
          <button
            key={p}
            role="tab"
            aria-selected={effective === p}
            className={`time-tab ${effective === p ? 'active' : ''}`}
            onClick={() => setPeriod(p)}
            data-testid={`earnings-chart-tab-${p}`}
          >
            {p}
          </button>
        ))}
      </div>

      {isEmpty ? (
        <div className="earnings-chart-empty" data-testid="earnings-chart-empty">
          No projection data yet
        </div>
      ) : (
        <div className="earnings-chart-svg-wrap" style={{ position: 'relative' }}>
          {isStale && (
            <span
              className="svgchart-stale-badge"
              tabIndex={0}
              role="status"
              aria-label="Earnings telemetry stale — no recent samples"
              data-tooltip="No recent telemetry samples have arrived for this chart. The last known points are still shown — they are not faked or extrapolated. Judge miner health by accepted shares, not by a chart that stopped updating."
              data-tooltip-pos="bottom"
            >
              Stale
            </span>
          )}
          <svg
            ref={svgRef}
            viewBox={`0 0 ${W} ${H}`}
            preserveAspectRatio="xMidYMid meet"
            className="earnings-chart-svg"
            onMouseMove={onMove}
            onMouseLeave={onLeave}
            role="img"
            aria-label={`Projected sats at current rate over ${effective}`}
          >
            <defs>
              <linearGradient id="earnings-fill" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stopColor="var(--accent, #FAA500)" stopOpacity="0.45" />
                <stop offset="100%" stopColor="var(--accent, #FAA500)" stopOpacity="0.02" />
              </linearGradient>
            </defs>

            {/* Y grid */}
            {yTicks.map((tickV, i) => {
              const sRange = Math.max(1, maxSats - minSats);
              const innerH = H - PAD_T - PAD_B;
              const y = PAD_T + innerH - ((tickV - minSats) / sRange) * innerH;
              return (
                <g key={`y-${i}`}>
                  <line
                    x1={PAD_L} x2={W - PAD_R} y1={y} y2={y}
                    stroke="var(--border, rgba(255,255,255,0.06))"
                    strokeWidth={1}
                    strokeDasharray={i === 0 ? '' : '2 3'}
                  />
                  <text
                    x={PAD_L - 8} y={y + 3}
                    textAnchor="end"
                    className="earnings-chart-axis-label"
                  >
                    {fmtSats(tickV)}
                  </text>
                </g>
              );
            })}

            {/* X tick labels */}
            {xTicks.map(i => {
              if (!sorted[i] || !pts[i]) return null;
              return (
                <text
                  key={`x-${i}`}
                  x={pts[i].x} y={H - 8}
                  textAnchor="middle"
                  className="earnings-chart-axis-label"
                >
                  {fmtTs(sorted[i].ts, effective)}
                </text>
              );
            })}

            {/* Area + line */}
            <path d={areaPath} fill="url(#earnings-fill)" />
            <path
              d={path}
              fill="none"
              stroke="var(--accent, #FAA500)"
              strokeWidth={2}
              strokeLinejoin="round"
              strokeLinecap="round"
            />

            {/* Hover marker */}
            {hoverIdx !== null && pts[hoverIdx] && (
              <g>
                <line
                  x1={pts[hoverIdx].x} x2={pts[hoverIdx].x}
                  y1={PAD_T} y2={H - PAD_B}
                  stroke="var(--accent, #FAA500)"
                  strokeWidth={1}
                  strokeOpacity={0.45}
                />
                <circle
                  cx={pts[hoverIdx].x} cy={pts[hoverIdx].y}
                  r={4}
                  fill="var(--accent, #FAA500)"
                  stroke="#000"
                  strokeWidth={1.5}
                />
              </g>
            )}
          </svg>

          {hoverIdx !== null && sorted[hoverIdx] && (
            <div
              className="chart-tooltip earnings-chart-tooltip"
              style={{
                left: `${(pts[hoverIdx].x / W) * 100}%`,
                top: `${(pts[hoverIdx].y / H) * 100}%`,
              }}
              data-testid="earnings-chart-tooltip"
            >
              <div className="earnings-chart-tooltip-ts">{fmtTs(sorted[hoverIdx].ts, effective)}</div>
              <div className="earnings-chart-tooltip-val">
                <span className="accent">{fmtSats(sorted[hoverIdx].sats)}</span> sats
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export default EarningsChart;
