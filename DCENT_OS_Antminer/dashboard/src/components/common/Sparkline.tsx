import React, { useId, useMemo, useRef, useState, useCallback } from 'react';

interface SparklineProps {
  /**
   * Sample values, oldest-first. NaN entries are treated as missing data
   * and rendered as visible discontinuities (no interpolation across gaps).
   */
  data: number[];
  width?: number;
  height?: number;
  color?: string;
  className?: string;
  /**
   * Render a soft accent gradient fill below the line. Default ON. Set
   * `false` for ultra-minimal inline tails.
   */
  fill?: boolean;
  /**
   * Render the last-point dot with a soft pulsing glow. Default ON.
   * Respects `prefers-reduced-motion` (pulse disabled, dot still shown).
   */
  lastPointDot?: boolean;
  /**
   * Show a CSS-only value tooltip on hover/touch. Default ON.
   * Pass `formatValue` to format the numeric value.
   */
  tooltip?: boolean;
  formatValue?: (v: number) => string;
  /**
   * Accessible summary for screen readers. When provided, the sparkline
   * is announced via `role="img"` + `aria-label={summaryText}`. When
   * absent (default) the SVG stays `aria-hidden="true"` — appropriate
   * for the dominant "paired with a sibling KPI value" usage where the
   * sparkline is decorative and the value is the announced truth.
   */
  summaryText?: string;
}

/**
 * Smooth-bezier sparkline tuned for the DCENT_OS firmware dashboard.
 *
 * Honesty contract: NaN samples render as gaps; we never interpolate
 * across missing data. Single-segment paths use a quadratic curve;
 * longer paths use a Catmull–Rom -> cubic-Bezier conversion clamped to
 * keep the curve inside the plot box (no overshoot below min/above max).
 */
function defaultFormat(v: number): string {
  const a = Math.abs(v);
  if (a >= 10000) return v.toFixed(0);
  if (a >= 100) return v.toFixed(0);
  if (a >= 10) return v.toFixed(1);
  if (a >= 1) return v.toFixed(2);
  return v.toFixed(3);
}

interface PathSegment {
  /** Smoothed cubic-bezier path for the line. */
  line: string;
  /** Area-fill path (line + base-to-floor closure). */
  area: string;
  /** Last `(x, y, value)` in this segment (used for the live dot). */
  lastX: number;
  lastY: number;
  lastValue: number;
}

/**
 * Catmull–Rom -> cubic-Bezier with chord-clamped tension, keeping the
 * spline inside the [yMin..yMax] band so we never overshoot the plot.
 */
function buildSegment(
  pts: Array<{ x: number; y: number; v: number }>,
  yFloor: number,
  yMin: number,
  yMax: number,
): PathSegment | null {
  if (pts.length === 0) return null;
  if (pts.length === 1) {
    const { x, y, v } = pts[0];
    return {
      line: `M${x.toFixed(1)},${y.toFixed(1)} L${x.toFixed(1)},${y.toFixed(1)}`,
      area: `M${x.toFixed(1)},${yFloor.toFixed(1)} L${x.toFixed(1)},${y.toFixed(1)} L${x.toFixed(1)},${yFloor.toFixed(1)} Z`,
      lastX: x,
      lastY: y,
      lastValue: v,
    };
  }

  const tension = 0.5;
  let line = `M${pts[0].x.toFixed(1)},${pts[0].y.toFixed(1)}`;
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[i - 1] ?? pts[i];
    const p1 = pts[i];
    const p2 = pts[i + 1];
    const p3 = pts[i + 2] ?? p2;
    const c1x = p1.x + ((p2.x - p0.x) / 6) * tension;
    let c1y = p1.y + ((p2.y - p0.y) / 6) * tension;
    const c2x = p2.x - ((p3.x - p1.x) / 6) * tension;
    let c2y = p2.y - ((p3.y - p1.y) / 6) * tension;
    if (c1y < yMin) c1y = yMin;
    if (c1y > yMax) c1y = yMax;
    if (c2y < yMin) c2y = yMin;
    if (c2y > yMax) c2y = yMax;
    line += ` C${c1x.toFixed(1)},${c1y.toFixed(1)} ${c2x.toFixed(1)},${c2y.toFixed(1)} ${p2.x.toFixed(1)},${p2.y.toFixed(1)}`;
  }

  const first = pts[0];
  const last = pts[pts.length - 1];
  const area =
    `M${first.x.toFixed(1)},${yFloor.toFixed(1)} ` +
    `L${first.x.toFixed(1)},${first.y.toFixed(1)} ` +
    line.slice(1) +
    ` L${last.x.toFixed(1)},${yFloor.toFixed(1)} Z`;

  return {
    line,
    area,
    lastX: last.x,
    lastY: last.y,
    lastValue: last.v,
  };
}

export const Sparkline = React.memo(function Sparkline({
  data,
  width = 80,
  height = 24,
  color = 'var(--accent, #FAA500)',
  className,
  fill = true,
  lastPointDot = true,
  tooltip = true,
  formatValue,
  summaryText,
}: SparklineProps) {
  const uid = useId().replace(/[:]/g, '_');
  const gradId = `sl-grad-${uid}`;
  const containerRef = useRef<HTMLSpanElement | null>(null);
  // Tooltip position is in CLIENT (rendered) pixels relative to the svg.
  // `rw`/`rh` capture the svg's rendered box so the clamp uses the same
  // unit as `x`/`y` — the svg may be CSS-scaled away from its viewBox size,
  // so clamping against the viewBox `width` prop would mis-place the tip.
  const [hover, setHover] = useState<{ x: number; y: number; v: number; rw: number; rh: number } | null>(null);

  const { segments, lastSeg, valid } = useMemo(() => {
    const n = data.length;
    const padding = 2;
    const plotH = Math.max(1, height - padding * 2);
    const plotW = Math.max(1, width - padding * 2);

    let max = -Infinity;
    let min = Infinity;
    let validCount = 0;
    for (let i = 0; i < n; i++) {
      const v = data[i];
      if (Number.isFinite(v)) {
        if (v > max) max = v;
        if (v < min) min = v;
        validCount++;
      }
    }

    if (validCount < 2 || !Number.isFinite(max) || !Number.isFinite(min)) {
      return { segments: [] as PathSegment[], lastSeg: null, valid: false };
    }
    const range = max - min || 1;

    const segs: PathSegment[] = [];
    let buf: Array<{ x: number; y: number; v: number }> = [];
    const yMin = padding;
    const yMax = padding + plotH;
    const flush = () => {
      const seg = buildSegment(buf, padding + plotH + 0.5, yMin, yMax);
      if (seg) segs.push(seg);
      buf = [];
    };
    for (let i = 0; i < n; i++) {
      const v = data[i];
      const x = padding + (i / Math.max(1, n - 1)) * plotW;
      if (!Number.isFinite(v)) {
        flush();
        continue;
      }
      const y = padding + plotH - ((v - min) / range) * plotH;
      buf.push({ x, y, v });
    }
    flush();

    return {
      segments: segs,
      lastSeg: segs[segs.length - 1] ?? null,
      valid: true,
    };
  }, [data, width, height]);

  const onMove = useCallback((e: React.MouseEvent<SVGSVGElement>) => {
    if (!tooltip || !valid) return;
    const svg = e.currentTarget;
    const rect = svg.getBoundingClientRect();
    const scale = width / rect.width;
    const xSvg = (e.clientX - rect.left) * scale;
    const n = data.length;
    const padding = 2;
    const plotW = Math.max(1, width - padding * 2);
    const frac = Math.min(1, Math.max(0, (xSvg - padding) / plotW));
    let idx = Math.round(frac * (n - 1));
    let probe = idx;
    while (probe < n && !Number.isFinite(data[probe])) probe++;
    if (probe >= n) {
      probe = idx;
      while (probe >= 0 && !Number.isFinite(data[probe])) probe--;
    }
    if (probe < 0 || probe >= n || !Number.isFinite(data[probe])) {
      setHover(null);
      return;
    }
    idx = probe;
    setHover({
      x: e.clientX - rect.left,
      y: e.clientY - rect.top,
      v: data[idx],
      rw: rect.width,
      rh: rect.height,
    });
  }, [tooltip, valid, data, width]);

  const onLeave = useCallback(() => setHover(null), []);

  if (!valid) return null;

  const fmt = formatValue ?? defaultFormat;

  return (
    <span
      ref={containerRef}
      className={className ? `dcent-sparkline ${className}` : 'dcent-sparkline'}
    >
      <svg
        width={width}
        height={height}
        viewBox={`0 0 ${width} ${height}`}
        {...(summaryText
          ? { role: 'img' as const, 'aria-label': summaryText }
          : { 'aria-hidden': true as const })}
        focusable="false"
        onMouseMove={tooltip ? onMove : undefined}
        onMouseLeave={tooltip ? onLeave : undefined}
        className="dcent-sparkline-svg"
      >
        {fill && (
          <defs>
            <linearGradient id={gradId} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor={color} stopOpacity="0.32" />
              <stop offset="55%" stopColor={color} stopOpacity="0.10" />
              <stop offset="100%" stopColor={color} stopOpacity="0" />
            </linearGradient>
          </defs>
        )}
        {fill && segments.map((seg, i) => (
          <path key={`f${i}`} d={seg.area} fill={`url(#${gradId})`} stroke="none" />
        ))}
        {segments.map((seg, i) => (
          <path
            key={`l${i}`}
            d={seg.line}
            fill="none"
            stroke={color}
            strokeWidth={1.5}
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        ))}
        {lastPointDot && lastSeg && (
          <g>
            <circle
              cx={lastSeg.lastX}
              cy={lastSeg.lastY}
              r={3.4}
              fill="none"
              stroke={color}
              strokeOpacity={0.45}
              strokeWidth={1.2}
              className="sparkline-pulse"
            />
            <circle
              cx={lastSeg.lastX}
              cy={lastSeg.lastY}
              r={1.7}
              fill={color}
            />
          </g>
        )}
      </svg>
      {tooltip && hover && (
        <span
          role="tooltip"
          className="dcent-sparkline-tip"
          style={{
            // Clamp against the rendered svg box (client px), not the viewBox
            // `width` prop — the two differ whenever the svg is CSS-scaled.
            left: Math.max(0, Math.min(hover.x + 6, hover.rw - 50)),
            top: Math.max(-18, hover.y - 22),
          }}
        >
          {fmt(hover.v)}
        </span>
      )}
    </span>
  );
});
