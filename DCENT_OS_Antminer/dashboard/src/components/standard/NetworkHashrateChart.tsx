// NetworkHashrateChart — compact sparkline of BTC global network
// hashrate (exahash/s) over time. Below the sparkline: current value
// in big numbers + % change vs 30d ago.
//
// Two honest data modes:
//   • `data` — a real time series (oracle / dcentrald history) → sparkline
//     + 30d delta. Shown points are never extrapolated.
//   • `estimate` — a single on-device value derived from the live block
//     difficulty (network_hashrate ≈ difficulty · 2^32 / 600). A single
//     point cannot be a trend, so we render the big value with an explicit
//     "on-device estimate" caption and NO sparkline / NO fake 30d delta.
//
// Empty state (neither provided): "Connect to a network oracle in settings"
// with helpful caption. Pure SVG. No external dependencies.

import React, { useMemo } from 'react';

export interface NetworkHashratePoint {
  ts: number; // ms epoch
  eh: number; // exahash/s
}

/** A single on-device estimate (no trend) derived from block difficulty. */
export interface NetworkHashrateEstimate {
  eh: number; // exahash/s
}

interface Props {
  data: NetworkHashratePoint[];
  /**
   * Optional on-device estimate, used ONLY when `data` has no real series.
   * Rendered as a single honest value labeled an estimate — never charted
   * as a trend and never used to compute a 30d delta.
   */
  estimate?: NetworkHashrateEstimate | null;
}

const W = 220;
const H = 56;
const PAD = 4;

// Stale cue: newest network-hashrate sample older than this means the oracle
// feed stopped flowing. Shown points stay real — never extrapolated.
const STALE_AFTER_MS = 60 * 60 * 1000; // 1h (network hashrate moves slowly)

function fmtEh(eh: number): string {
  if (eh >= 1000) return `${(eh / 1000).toFixed(2)} ZH`;
  if (eh >= 100) return `${eh.toFixed(0)} EH`;
  return `${eh.toFixed(1)} EH`;
}

function fmtPct(p: number): string {
  const sign = p > 0 ? '+' : '';
  return `${sign}${p.toFixed(1)}%`;
}

// Catmull–Rom → cubic-Bezier smoothing (same technique as EarningsChart's
// smoothPath), with control points clamped to the plot band so the curve
// never overshoots the real data range.
function smoothPath(
  pts: Array<{ x: number; y: number }>,
  yMin: number,
  yMax: number,
): string {
  if (pts.length === 0) return '';
  if (pts.length === 1) return `M ${pts[0].x.toFixed(2)} ${pts[0].y.toFixed(2)}`;
  let d = `M ${pts[0].x.toFixed(2)} ${pts[0].y.toFixed(2)}`;
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
    d += ` C ${cp1x.toFixed(2)} ${cp1y.toFixed(2)}, ${cp2x.toFixed(2)} ${cp2y.toFixed(2)}, ${p2.x.toFixed(2)} ${p2.y.toFixed(2)}`;
  }
  return d;
}

export function NetworkHashrateChart({ data, estimate = null }: Props) {
  const sorted = useMemo(
    () => [...data].sort((a, b) => a.ts - b.ts),
    [data],
  );

  // A real series wins. Otherwise fall back to a single on-device estimate
  // (honest single value, no sparkline). Only when neither exists is the cell
  // truly empty.
  const hasSeries = sorted.length > 0;
  const estimateEh =
    estimate && Number.isFinite(estimate.eh) && estimate.eh > 0 ? estimate.eh : null;
  const isEmpty = !hasSeries && estimateEh === null;

  const { path, current, pct30d, isStale } = useMemo(() => {
    if (sorted.length === 0) {
      return { path: '', current: 0, pct30d: 0, isStale: false };
    }
    const min = Math.min(...sorted.map(p => p.eh));
    const max = Math.max(...sorted.map(p => p.eh));
    const range = Math.max(1e-6, max - min);
    const innerW = W - PAD * 2;
    const innerH = H - PAD * 2;
    const minTs = sorted[0].ts;
    const maxTs = sorted[sorted.length - 1].ts;
    const tRange = Math.max(1, maxTs - minTs);
    const yMin = PAD;
    const yMax = PAD + innerH;
    const pts = sorted.map(p => ({
      x: PAD + ((p.ts - minTs) / tRange) * innerW,
      y: PAD + innerH - ((p.eh - min) / range) * innerH,
    }));
    const d = smoothPath(pts, yMin, yMax);
    const _current = sorted[sorted.length - 1].eh;
    // Find a sample ~30 days ago.
    const target = maxTs - 30 * 86400 * 1000;
    let oldest = sorted[0];
    for (const p of sorted) {
      if (p.ts <= target) oldest = p; else break;
    }
    const _pct = oldest.eh > 0 ? ((_current - oldest.eh) / oldest.eh) * 100 : 0;
    const _stale = Date.now() - maxTs > STALE_AFTER_MS;
    return { path: d, current: _current, pct30d: _pct, isStale: _stale };
  }, [sorted]);

  return (
    <div
      className="network-hashrate-chart"
      data-testid="network-hashrate-chart"
      style={{ position: 'relative' }}
    >
      {isEmpty ? (
        <div className="network-hashrate-chart-empty" data-testid="network-hashrate-chart-empty">
          <div className="network-hashrate-chart-empty-title">
            Connect to a network oracle in settings
          </div>
          <div className="network-hashrate-chart-empty-caption">
            Network hashrate is derived from on-device difficulty + block interval
            once dcentrald exposes the value, or by enabling a read-only fee/hash
            oracle (e.g. mempool.space) in Settings.
          </div>
        </div>
      ) : !hasSeries ? (
        // Single on-device estimate (difficulty · 2^32 / 600). One value is not
        // a trend, so render the big number with an explicit estimate caption —
        // no sparkline, no fabricated 30d delta.
        <div
          className="network-hashrate-chart-estimate"
          data-testid="network-hashrate-chart-estimate"
        >
          <div className="network-hashrate-chart-readout">
            <div
              className="network-hashrate-chart-value"
              data-testid="network-hashrate-chart-value"
            >
              {fmtEh(estimateEh!)}
              <span className="network-hashrate-chart-unit">/s</span>
            </div>
          </div>
          <div className="network-hashrate-chart-empty-caption">
            On-device estimate from current difficulty (≈ difficulty · 2³² ÷ 600 s).
            Connect a read-only hash oracle for a measured trend.
          </div>
        </div>
      ) : (
        <>
          {isStale && (
            <span
              className="svgchart-stale-badge"
              tabIndex={0}
              role="status"
              aria-label="Network hashrate telemetry stale — no recent samples"
              data-tooltip="No recent network-hashrate samples have arrived from the oracle. The last known points are still shown — they are not faked or extrapolated."
              data-tooltip-pos="bottom"
            >
              Stale
            </span>
          )}
          {/* preserveAspectRatio="none" is intentional here: this is a thin
              full-width strip (CSS width:100% × fixed 56px) containing ONLY a
              non-scaling-stroke <path> — no text/circles to distort — so "none"
              fills the strip edge-to-edge. "meet" would letterbox it to a tiny
              centered chart on wide cards. Do NOT switch to meet. */}
          <svg
            viewBox={`0 0 ${W} ${H}`}
            preserveAspectRatio="none"
            className="network-hashrate-chart-svg"
            role="img"
            aria-label="Network hashrate sparkline"
          >
            <defs>
              <linearGradient id="nethash-grad" x1="0" y1="0" x2="1" y2="0">
                <stop offset="0%" stopColor="var(--accent, #FAA500)" stopOpacity="0.5" />
                <stop offset="100%" stopColor="var(--accent, #FAA500)" stopOpacity="1" />
              </linearGradient>
            </defs>
            <path
              d={path}
              fill="none"
              stroke="url(#nethash-grad)"
              strokeWidth={1.8}
              strokeLinejoin="round"
              strokeLinecap="round"
              vectorEffect="non-scaling-stroke"
            />
          </svg>
          <div className="network-hashrate-chart-readout">
            <div
              className="network-hashrate-chart-value"
              data-testid="network-hashrate-chart-value"
            >
              {fmtEh(current)}
              <span className="network-hashrate-chart-unit">/s</span>
            </div>
            <div
              className={`network-hashrate-chart-delta ${pct30d >= 0 ? 'pos' : 'neg'}`}
              data-testid="network-hashrate-chart-delta"
            >
              {fmtPct(pct30d)}
              <span className="network-hashrate-chart-delta-label">vs 30d</span>
            </div>
          </div>
        </>
      )}
    </div>
  );
}

export default NetworkHashrateChart;
