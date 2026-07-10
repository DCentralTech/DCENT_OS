// MempoolFeeRadial — 180° semicircle fee gauge with three colored arcs
// (green / yellow / red) and a needle pointing at the current `fastest`
// fee value. Below the gauge: 3-line summary "FAST 47 / 30M 32 / 1H 18"
// sat/vB. Em-dash empty state when fees === null.
//
// Pure SVG. No external dependencies.

import React from 'react';

const DASH = '—';

export interface MempoolFees {
  fastest: number | null;
  halfHour: number | null;
  hour: number | null;
}

interface Props {
  fees: MempoolFees | null;
}

// Gauge geometry.
const W = 220;
const H = 130;
const CX = W / 2;
const CY = H - 16; // baseline of the arc
const R_OUT = 92;
const R_IN = 68;
const STROKE = R_OUT - R_IN; // arc thickness in stroke-width terms
const R_MID = (R_OUT + R_IN) / 2;

// Domain: 0 → 120 sat/vB clamped. Bands 0-20 green, 20-80 yellow, 80+ red.
const MAX_FEE = 120;
const BAND_LOW = 20;
const BAND_MED = 80;

function clamp(v: number, lo: number, hi: number) {
  return Math.max(lo, Math.min(hi, v));
}

// Map a fee value to an angle in degrees, where the arc spans 180° → 0°
// (left → right).
function feeToAngle(fee: number): number {
  const norm = clamp(fee, 0, MAX_FEE) / MAX_FEE;
  return 180 - norm * 180;
}

function polar(angleDeg: number, r: number) {
  // Standard gauge polar: 180° = left point, 90° = top, 0° = right point
  // (y grows downward). `+cos` keeps the angle convention un-mirrored so the
  // arc sweep below draws cleanly over the top.
  const a = (angleDeg * Math.PI) / 180;
  return { x: CX + r * Math.cos(a), y: CY - r * Math.sin(a) };
}

function arcPath(startFee: number, endFee: number, r: number): string {
  const a1 = feeToAngle(startFee); // larger angle (further left / lower fee)
  const a2 = feeToAngle(endFee); // smaller angle (further right / higher fee)
  const p1 = polar(a1, r);
  const p2 = polar(a2, r);
  const largeArc = Math.abs(a1 - a2) > 180 ? 1 : 0;
  // sweep-flag 1 traces the UPPER semicircle: as fee rises the angle DECREASES
  // (180°→0°), which is clockwise on screen (left → over the top → right) in
  // SVG's y-down space. Adjacent bands share endpoints and tile cleanly.
  return `M ${p1.x} ${p1.y} A ${r} ${r} 0 ${largeArc} 1 ${p2.x} ${p2.y}`;
}

export function MempoolFeeRadial({ fees }: Props) {
  const hasData = fees != null && fees.fastest !== null;
  const fastest = fees?.fastest ?? null;
  const halfHour = fees?.halfHour ?? null;
  const hour = fees?.hour ?? null;

  const needleAngle = hasData ? feeToAngle(fastest as number) : 180;
  const needleTip = polar(needleAngle, R_OUT - 4);
  const needleBase1 = polar(needleAngle + 90, 5);
  const needleBase2 = polar(needleAngle - 90, 5);

  return (
    <div className="mempool-fee-radial" data-testid="mempool-fee-radial">
      <svg
        viewBox={`0 0 ${W} ${H}`}
        preserveAspectRatio="xMidYMax meet"
        className="mempool-fee-radial-svg"
        role="img"
        aria-label="Mempool fee gauge"
      >
        {/* Background track */}
        <path
          d={arcPath(0, MAX_FEE, R_MID)}
          fill="none"
          stroke="var(--border, rgba(255,255,255,0.06))"
          strokeWidth={STROKE}
          strokeLinecap="butt"
        />
        {/* Green band */}
        <path
          d={arcPath(0, BAND_LOW, R_MID)}
          fill="none"
          stroke="var(--green, #2DD4A0)"
          strokeWidth={STROKE}
          strokeLinecap="butt"
          opacity={hasData ? 0.95 : 0.45}
        />
        {/* Yellow band */}
        <path
          d={arcPath(BAND_LOW, BAND_MED, R_MID)}
          fill="none"
          stroke="var(--yellow, #FACC15)"
          strokeWidth={STROKE}
          strokeLinecap="butt"
          opacity={hasData ? 0.95 : 0.45}
        />
        {/* Red band */}
        <path
          d={arcPath(BAND_MED, MAX_FEE, R_MID)}
          fill="none"
          stroke="var(--red, #FF6B6B)"
          strokeWidth={STROKE}
          strokeLinecap="butt"
          opacity={hasData ? 0.95 : 0.45}
        />

        {/* Tick marks at the band boundaries. */}
        {[0, BAND_LOW, BAND_MED, MAX_FEE].map(t => {
          const a = feeToAngle(t);
          const p1 = polar(a, R_IN - 2);
          const p2 = polar(a, R_OUT + 2);
          return (
            <line
              key={`tick-${t}`}
              x1={p1.x} y1={p1.y}
              x2={p2.x} y2={p2.y}
              stroke="var(--text-dim, #6b6b80)"
              strokeWidth={1}
            />
          );
        })}

        {/* Secondary markers: 30M (half-hour) + 1H (hour) fee values shown as
            small dots on the arc, so all three rates read at a glance and the
            needle (fastest) isn't the only datapoint on the gauge. */}
        {hasData && halfHour !== null && (
          (() => {
            const p = polar(feeToAngle(halfHour), R_MID);
            return (
              <g data-testid="mempool-fee-radial-marker-30m">
                <circle cx={p.x} cy={p.y} r={3.2} fill="var(--text, #fff)" stroke="#000" strokeWidth={0.8} opacity={0.85} />
              </g>
            );
          })()
        )}
        {hasData && hour !== null && (
          (() => {
            const p = polar(feeToAngle(hour), R_MID);
            return (
              <g data-testid="mempool-fee-radial-marker-1h">
                <circle cx={p.x} cy={p.y} r={2.6} fill="var(--text-dim, #6b6b80)" stroke="#000" strokeWidth={0.8} opacity={0.85} />
              </g>
            );
          })()
        )}

        {/* Needle */}
        {hasData && (
          <g data-testid="mempool-fee-radial-needle">
            <polygon
              points={`${needleTip.x},${needleTip.y} ${needleBase1.x},${needleBase1.y} ${needleBase2.x},${needleBase2.y}`}
              fill="var(--text, #fff)"
              stroke="#000"
              strokeWidth={0.8}
            />
            <circle cx={CX} cy={CY} r={5} fill="var(--accent, #FAA500)" stroke="#000" strokeWidth={1} />
          </g>
        )}

        {/* Center value */}
        <text
          x={CX} y={CY - 10}
          textAnchor="middle"
          className="mempool-fee-radial-center"
        >
          {hasData ? `${Math.round(fastest as number)}` : DASH}
        </text>
        <text
          x={CX} y={CY + 4}
          textAnchor="middle"
          className="mempool-fee-radial-center-unit"
        >
          sat/vB
        </text>
      </svg>

      <div className="mempool-fee-radial-summary" data-testid="mempool-fee-radial-summary">
        <div className="mempool-fee-radial-summary-row">
          <span className="mempool-fee-radial-summary-label">FAST</span>
          <span className="mempool-fee-radial-summary-val">{fastest !== null ? Math.round(fastest) : DASH}</span>
        </div>
        <div className="mempool-fee-radial-summary-row">
          <span className="mempool-fee-radial-summary-label">30M</span>
          <span className="mempool-fee-radial-summary-val">{halfHour !== null ? Math.round(halfHour) : DASH}</span>
        </div>
        <div className="mempool-fee-radial-summary-row">
          <span className="mempool-fee-radial-summary-label">1H</span>
          <span className="mempool-fee-radial-summary-val">{hour !== null ? Math.round(hour) : DASH}</span>
        </div>
      </div>
    </div>
  );
}

export default MempoolFeeRadial;
