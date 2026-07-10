import React, { useState, useCallback, useRef, useMemo, useId, useEffect } from 'react';
import { glossaryText } from '../../utils/glossary';

export interface DataPoint {
  time: number;  // unix timestamp in seconds
  value: number;
}

export interface ChartSeries {
  data: DataPoint[];
  color: string;
  label?: string;
  dashed?: boolean;
  yAxis?: 'left' | 'right';
  /**
   * Optional area-fill under the line. Defaults to TRUE for the first
   * left-axis series, FALSE for everything else (so a hashrate primary
   * series gets the premium gradient and overlay series stay readable).
   */
  fill?: boolean;
}

interface SvgChartProps {
  series: ChartSeries[];
  height?: number;
  showGrid?: boolean;
  showXAxis?: boolean;
  showYAxis?: boolean;
  formatValue?: (value: number, seriesIndex: number) => string;
  formatTime?: (time: number) => string;
  className?: string;
  style?: React.CSSProperties;
  /**
   * Stale threshold in seconds. When the newest sample across all series
   * is older than this, a "STALE" badge is rendered in the top-right.
   * Pass 0 or negative to disable. Default 120s.
   */
  staleAfterSec?: number;
  /**
   * Empty-state placeholder text. Default "No data yet".
   */
  emptyText?: string;
  /**
   * Accessible summary for screen readers. When provided, the SVG is
   * announced via `role="img"` + `aria-label={summaryText}`. When
   * absent (default), the chart stays `aria-hidden="true"` —
   * appropriate when the chart is paired with a sibling KPI block
   * that carries the announced truth.
   */
  summaryText?: string;
  /**
   * Glossary key whose canonical text explains the STALE badge on hover
   * (CSS `[data-tooltip]` path). Additive, safe default — resolves to the
   * F6-frozen `telemetry_stale` glossary key ( §6), which is the
   * truth-contract "telemetry stopped flowing — last points are real, not
   * faked/extrapolated; judge health by accepted shares" explainer.
   *
   * TRUTH-CONTRACT: this is ONLY the on-hover explanation. It is never used
   * to hide or soften the STALE badge — the badge itself stays a
   * high-contrast, always-visible warning. If the glossary key is ever
   * missing (`glossaryText` → ''), the verbatim  inline string is
   * used as a hard fallback so the honest explainer can never silently
   * vanish. Callers may override with any other glossary key.
   *
   * Default: `'telemetry_stale'` ( P6 — replaces the  inline
   * literal default; the F6 key body is byte-faithful in meaning).
   */
  staleTooltipTerm?: string;
  drawIdentity?: string;
}

// Nice tick values for y-axis labels
function niceNum(range: number, round: boolean): number {
  const exp = Math.floor(Math.log10(range));
  const frac = range / Math.pow(10, exp);
  let nice: number;
  if (round) {
    if (frac < 1.5) nice = 1;
    else if (frac < 3) nice = 2;
    else if (frac < 7) nice = 5;
    else nice = 10;
  } else {
    if (frac <= 1) nice = 1;
    else if (frac <= 2) nice = 2;
    else if (frac <= 5) nice = 5;
    else nice = 10;
  }
  return nice * Math.pow(10, exp);
}

function niceScale(min: number, max: number, ticks: number): { min: number; max: number; step: number } {
  if (min === max) {
    const margin = Math.abs(min) * 0.04 || 0.5;
    const step = margin / 2;
    return { min: min - margin, max: max + margin, step };
  }
  const span = max - min;
  const pad = span * 0.15;
  const paddedMin = min - pad;
  const paddedMax = max + pad;
  const step = niceNum((paddedMax - paddedMin) / (ticks - 1), true);
  const niceMin = Math.floor(paddedMin / step) * step;
  const niceMax = Math.ceil(paddedMax / step) * step;
  return { min: niceMin, max: niceMax, step };
}

function defaultFormatValue(val: number): string {
  const abs = Math.abs(val);
  if (abs >= 10000) return val.toFixed(0);
  if (abs >= 100) return val.toFixed(0);
  if (abs >= 10) return val.toFixed(1);
  if (abs >= 1) return val.toFixed(1);
  return val.toFixed(2);
}

function defaultFormatTime(t: number): string {
  const d = new Date(t * 1000);
  return `${d.getHours().toString().padStart(2, '0')}:${d.getMinutes().toString().padStart(2, '0')}`;
}

/**
 * Build a Catmull–Rom -> cubic-Bezier smoothed path for a series of (x,y)
 * points. Control points are clamped to the plot band so the smoothing
 * never overshoots the actual data range.
 */
function smoothPath(
  pts: Array<{ x: number; y: number }>,
  yMin: number,
  yMax: number,
): string {
  if (pts.length === 0) return '';
  if (pts.length === 1) {
    const { x, y } = pts[0];
    return `M${x.toFixed(2)},${y.toFixed(2)}`;
  }
  const tension = 0.5;
  let d = `M${pts[0].x.toFixed(2)},${pts[0].y.toFixed(2)}`;
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
    d += ` C${c1x.toFixed(2)},${c1y.toFixed(2)} ${c2x.toFixed(2)},${c2y.toFixed(2)} ${p2.x.toFixed(2)},${p2.y.toFixed(2)}`;
  }
  return d;
}

export const SvgChart = React.memo(function SvgChart({
  series,
  height = 240,
  showGrid = true,
  showXAxis = true,
  showYAxis = true,
  formatValue,
  formatTime,
  className,
  style,
  staleAfterSec = 120,
  emptyText = 'No data yet',
  summaryText,
  staleTooltipTerm = 'telemetry_stale',
  drawIdentity,
}: SvgChartProps) {
  // ARIA contract: when summaryText is provided, announce via role="img" +
  // aria-label; otherwise stay hidden from AT (paired with sibling KPI).
  const ariaProps: React.SVGProps<SVGSVGElement> = summaryText
    ? { role: 'img', 'aria-label': summaryText }
    : { 'aria-hidden': true };
  const containerRef = useRef<HTMLDivElement>(null);
  const uid = useId().replace(/[:]/g, '_');
  const [hoverInfo, setHoverInfo] = useState<{
    svgX: number;
    mouseX: number;
    mouseY: number;
  } | null>(null);

  const hasRightAxis = series.some(s => s.yAxis === 'right');
  const padding = useMemo(() => ({
    top: 12,
    right: hasRightAxis ? 50 : 16,
    bottom: 24,
    // : 50 -> 58 so 6-char Y-axis labels (e.g. "125000") at fontSize 10
    // don't clip against the left viewBox edge.
    left: 58,
  }), [hasRightAxis]);

  const chartWidth = 600;
  const chartHeight = height;
  const plotWidth = chartWidth - padding.left - padding.right;
  const plotHeight = chartHeight - padding.top - padding.bottom;

  const activeSeries = useMemo(() => series.filter(s => s.data.length > 0), [series]);

  // Exhaustive-state guard ( P6): a series can carry timestamps but
  // every value be non-finite (all-NaN — e.g. a sensor that never reported).
  // Without this, the chart would render axes + grid but no line and no
  // message, which is indistinguishable from a broken chart. Treat
  // "data present but zero finite values" as an honest empty state.
  const hasFiniteValue = useMemo(
    () => activeSeries.some(s => s.data.some(d => Number.isFinite(d.value))),
    [activeSeries],
  );

  const drawnIdentitiesRef = useRef<Set<string>>(new Set());
  const seriesDrawIdentity = useMemo(
    () => activeSeries
      .map(s => `${s.label ?? ''}:${s.color}:${s.yAxis ?? 'left'}:${s.dashed ? 'dashed' : 'solid'}:${s.fill === true ? 'fill' : 'line'}`)
      .join('|'),
    [activeSeries],
  );
  const activeDrawIdentity = drawIdentity ?? seriesDrawIdentity;
  const shouldDrawLines = activeDrawIdentity.length > 0
    && !drawnIdentitiesRef.current.has(activeDrawIdentity);
  useEffect(() => {
    if (activeDrawIdentity) {
      drawnIdentitiesRef.current.add(activeDrawIdentity);
    }
  }, [activeDrawIdentity]);

  const { timeMin, timeMax, timeRange } = useMemo(() => {
    if (activeSeries.length === 0) return { timeMin: 0, timeMax: 1, timeRange: 1 };
    let tMin = Infinity;
    let tMax = -Infinity;
    for (const s of activeSeries) {
      for (const d of s.data) {
        if (d.time < tMin) tMin = d.time;
        if (d.time > tMax) tMax = d.time;
      }
    }
    const range = tMax - tMin || 1;
    return { timeMin: tMin, timeMax: tMax, timeRange: range };
  }, [activeSeries]);

  const computeRange = useCallback((ss: ChartSeries[]) => {
    if (ss.length === 0) return { min: 0, max: 1, step: 0.25 };
    let vMin = Infinity;
    let vMax = -Infinity;
    for (const s of ss) {
      for (const d of s.data) {
        if (d.value < vMin) vMin = d.value;
        if (d.value > vMax) vMax = d.value;
      }
    }
    if (!isFinite(vMin)) return { min: 0, max: 1, step: 0.25 };
    return niceScale(vMin, vMax, 5);
  }, []);

  const leftSeries = useMemo(() => activeSeries.filter(s => s.yAxis !== 'right'), [activeSeries]);
  const rightSeries = useMemo(() => activeSeries.filter(s => s.yAxis === 'right'), [activeSeries]);
  const leftRange = useMemo(() => computeRange(leftSeries), [leftSeries, computeRange]);
  const rightRange = useMemo(() => computeRange(rightSeries), [rightSeries, computeRange]);

  const toSvgX = useCallback((time: number) => {
    return padding.left + ((time - timeMin) / timeRange) * plotWidth;
  }, [padding.left, timeMin, timeRange, plotWidth]);

  const toSvgY = useCallback((value: number, axis: 'left' | 'right') => {
    const range = axis === 'right' ? rightRange : leftRange;
    const rangeSpan = range.max - range.min || 1;
    return padding.top + plotHeight - ((value - range.min) / rangeSpan) * plotHeight;
  }, [padding.top, plotHeight, leftRange, rightRange]);

  // Render each series as smoothed segments split on NaN/non-finite gaps,
  // plus an optional gradient fill under the line.
  const seriesElements = useMemo(() => {
    const yMin = padding.top;
    const yMax = padding.top + plotHeight;
    const elems: React.ReactElement[] = [];

    activeSeries.forEach((s, i) => {
      if (s.data.length === 0) return;
      const axis = s.yAxis ?? 'left';
      const isFirstLeft = i === 0 && axis === 'left';
      const wantsFill = s.fill ?? isFirstLeft;
      // The primary curve gets the soft phosphor glow; overlay/dashed series
      // stay crisp + un-glowed so they never visually fight the headline trend.
      const wantsGlow = isFirstLeft && !s.dashed;

      // Single data point: render as a circle.
      if (s.data.length === 1) {
        const x = toSvgX(s.data[0].time);
        const y = toSvgY(s.data[0].value, axis);
        elems.push(<circle key={`pt${i}`} cx={x} cy={y} r={3} fill={s.color} />);
        return;
      }

      // Split into contiguous segments on non-finite values.
      const segs: Array<Array<{ x: number; y: number }>> = [];
      let buf: Array<{ x: number; y: number }> = [];
      const flush = () => { if (buf.length) { segs.push(buf); buf = []; } };
      for (const d of s.data) {
        if (!Number.isFinite(d.value)) { flush(); continue; }
        buf.push({ x: toSvgX(d.time), y: toSvgY(d.value, axis) });
      }
      flush();

      segs.forEach((seg, sidx) => {
        if (seg.length === 0) return;
        const linePath = smoothPath(seg, yMin, yMax);
        if (wantsFill && seg.length >= 2) {
          const first = seg[0];
          const last = seg[seg.length - 1];
          const areaPath =
            `M${first.x.toFixed(2)},${yMax.toFixed(2)} ` +
            `L${first.x.toFixed(2)},${first.y.toFixed(2)} ` +
            linePath.slice(1) +
            ` L${last.x.toFixed(2)},${yMax.toFixed(2)} Z`;
          elems.push(
            <path
              key={`area${i}_${sidx}`}
              d={areaPath}
              fill={`url(#svgchart-grad-${uid}-${i})`}
              stroke="none"
            />,
          );
        }
        elems.push(
          <path
            key={`line${i}_${sidx}`}
            d={linePath}
            fill="none"
            stroke={s.color}
            strokeWidth={wantsGlow ? 2 : 1.7}
            strokeLinejoin="round"
            strokeLinecap="round"
            strokeDasharray={s.dashed ? '4 4' : undefined}
            className="svgchart-line"
            data-draw={shouldDrawLines ? 'true' : undefined}
            filter={wantsGlow ? `url(#svgchart-glow-${uid})` : undefined}
            pathLength={1}
            style={{
              ['--svgchart-line-anim-delay' as never]: `${sidx * 60}ms`,
            } as React.CSSProperties}
          />,
        );
      });
    });

    return elems;
  }, [activeSeries, toSvgX, toSvgY, padding, plotHeight, shouldDrawLines, uid]);

  const gridElements = useMemo(() => {
    if (!showGrid) return null;
    const lines: React.ReactElement[] = [];
    const range = leftRange;
    // Clamp the line count: a pathological (min,max,step) can otherwise emit
    // dozens of grid lines and tank render perf. 12 is plenty for a chart.
    const steps = Math.min(12, Math.round((range.max - range.min) / range.step));
    for (let i = 0; i <= steps; i++) {
      const val = range.min + range.step * i;
      const y = toSvgY(val, 'left');
      if (y >= padding.top - 1 && y <= padding.top + plotHeight + 1) {
        lines.push(
          <line
            key={`h${i}`}
            x1={padding.left} y1={y}
            x2={chartWidth - padding.right} y2={y}
            stroke="rgba(255,255,255,0.05)"
            strokeWidth={1}
            shapeRendering="crispEdges"
          />,
        );
      }
    }
    return lines;
  }, [showGrid, leftRange, toSvgY, padding, plotHeight, chartWidth]);

  // Index of the first right-axis series, so right-axis tick labels are
  // formatted with that series' real unit (formatValue is keyed by series
  // index). Falls back to 1 if no right-axis series exists.
  const firstRightSeriesIndex = useMemo(() => {
    const idx = activeSeries.findIndex(s => s.yAxis === 'right');
    return idx >= 0 ? idx : 1;
  }, [activeSeries]);

  const yAxisLabels = useMemo(() => {
    if (!showYAxis) return null;
    const labels: React.ReactElement[] = [];

    const lSteps = Math.min(12, Math.round((leftRange.max - leftRange.min) / leftRange.step));
    for (let i = 0; i <= lSteps; i++) {
      const val = leftRange.min + leftRange.step * i;
      const y = toSvgY(val, 'left');
      if (y >= padding.top - 1 && y <= padding.top + plotHeight + 1) {
        const label = formatValue ? formatValue(val, 0) : defaultFormatValue(val);
        // Light tick mark
        labels.push(
          <line
            key={`tickL${i}`}
            x1={padding.left - 3} y1={y}
            x2={padding.left} y2={y}
            stroke="rgba(255,255,255,0.18)"
            strokeWidth={1}
            shapeRendering="crispEdges"
          />,
        );
        labels.push(
          <text
            key={`yl${i}`}
            x={padding.left - 6}
            y={y + 3.5}
            textAnchor="end"
            fill="var(--text-dim, #6E6E80)"
            fontSize="10"
            fontFamily="var(--font-mono, 'JetBrains Mono', monospace)"
          >
            {label}
          </text>,
        );
      }
    }

    if (hasRightAxis) {
      const rSteps = Math.min(12, Math.round((rightRange.max - rightRange.min) / rightRange.step));
      for (let i = 0; i <= rSteps; i++) {
        const val = rightRange.min + rightRange.step * i;
        const y = toSvgY(val, 'right');
        if (y >= padding.top - 1 && y <= padding.top + plotHeight + 1) {
          const label = formatValue ? formatValue(val, firstRightSeriesIndex) : defaultFormatValue(val);
          labels.push(
            <line
              key={`tickR${i}`}
              x1={chartWidth - padding.right} y1={y}
              x2={chartWidth - padding.right + 3} y2={y}
              stroke="rgba(255,255,255,0.18)"
              strokeWidth={1}
              shapeRendering="crispEdges"
            />,
          );
          labels.push(
            <text
              key={`yr${i}`}
              x={chartWidth - padding.right + 6}
              y={y + 3.5}
              textAnchor="start"
              fill="var(--text-dim, #6E6E80)"
              fontSize="10"
              fontFamily="var(--font-mono, 'JetBrains Mono', monospace)"
            >
              {label}
            </text>,
          );
        }
      }
    }

    return labels;
  }, [showYAxis, leftRange, rightRange, hasRightAxis, toSvgY, padding, plotHeight, chartWidth, formatValue, firstRightSeriesIndex]);

  const xAxisLabels = useMemo(() => {
    if (!showXAxis) return null;
    const fmt = formatTime || defaultFormatTime;
    const labels: React.ReactElement[] = [];
    const count = 5;
    const SYNTHETIC_SPAN = count * 60;
    const degenerate = timeRange < count;
    const effectiveSpan = degenerate ? SYNTHETIC_SPAN : timeRange;
    const effectiveStart = degenerate ? timeMax - SYNTHETIC_SPAN : timeMin;
    for (let i = 0; i <= count; i++) {
      const x = padding.left + (plotWidth / count) * i;
      const t = effectiveStart + (effectiveSpan / count) * i;
      labels.push(
        <text
          key={`xl${i}`}
          x={x}
          y={chartHeight - 4}
          textAnchor="middle"
          fill="var(--text-dim, #6E6E80)"
          fontSize="10"
          fontFamily="var(--font-mono, 'JetBrains Mono', monospace)"
        >
          {fmt(t)}
        </text>,
      );
    }
    return labels;
  }, [showXAxis, formatTime, padding.left, plotWidth, timeMin, timeMax, timeRange, chartHeight]);

  const handleMouseMove = useCallback((e: React.MouseEvent<SVGSVGElement>) => {
    const svg = e.currentTarget;
    const rect = svg.getBoundingClientRect();
    const scaleX = chartWidth / rect.width;
    const svgX = (e.clientX - rect.left) * scaleX;
    if (svgX >= padding.left && svgX <= chartWidth - padding.right) {
      setHoverInfo({
        svgX,
        mouseX: e.clientX - rect.left,
        mouseY: e.clientY - rect.top,
      });
    } else {
      setHoverInfo(null);
    }
  }, [chartWidth, padding.left, padding.right]);

  const handleMouseLeave = useCallback(() => setHoverInfo(null), []);

  // Tooltip data + per-series hover markers.
  //
  // SHARED nearest-index: a single `hoverTime` (from the crosshair X) drives
  // the nearest sample lookup for every series, and the marker dots all SNAP
  // to the crosshair X (`hoverInfo.svgX`) so they line up vertically on the
  // crosshair instead of scattering to each series' own sample X. The tooltip
  // header time uses the first series' nearest sample so the readout label and
  // the highlighted points refer to the same real moment.
  const tooltipData = useMemo(() => {
    if (!hoverInfo || activeSeries.length === 0) return null;
    const hoverTime = timeMin + ((hoverInfo.svgX - padding.left) / plotWidth) * timeRange;
    const fmt = formatTime || defaultFormatTime;
    const markerX = hoverInfo.svgX;

    let headerTime = hoverTime;
    let headerSet = false;

    const values = activeSeries.map((s, si) => {
      let nearest = s.data[0];
      let minDist = Math.abs(s.data[0].time - hoverTime);
      for (let j = 1; j < s.data.length; j++) {
        const dist = Math.abs(s.data[j].time - hoverTime);
        if (dist < minDist) { minDist = dist; nearest = s.data[j]; }
      }
      if (!headerSet) { headerTime = nearest.time; headerSet = true; }
      const axis = s.yAxis ?? 'left';
      const valStr = formatValue ? formatValue(nearest.value, si) : defaultFormatValue(nearest.value);
      const visible = Number.isFinite(nearest.value);
      return {
        label: s.label || `Series ${si + 1}`,
        value: valStr,
        color: s.color,
        // Snap marker to the crosshair X so all series markers align.
        pointX: markerX,
        pointY: visible ? toSvgY(nearest.value, axis) : NaN,
        visible,
      };
    });

    return { time: fmt(headerTime), values };
  }, [hoverInfo, activeSeries, timeMin, timeRange, padding.left, plotWidth, formatValue, formatTime, toSvgY]);

  // Stale-data detection.
  const isStale = useMemo(() => {
    if (!staleAfterSec || staleAfterSec <= 0 || activeSeries.length === 0) return false;
    const ageSec = Date.now() / 1000 - timeMax;
    return ageSec > staleAfterSec;
  }, [staleAfterSec, activeSeries.length, timeMax]);

  // STALE badge explainer text. Resolve the glossary key first (default
  // `telemetry_stale`, F6-frozen). TRUTH-CONTRACT HARD FALLBACK: if the key
  // is missing/empty (`glossaryText` → ''), use the verbatim  honest
  // inline string so the explainer can never silently disappear. The badge
  // itself is rendered unconditionally below regardless of this value.
  const staleTooltipText = useMemo(() => {
    const STALE_FALLBACK =
      'No recent telemetry samples have arrived for this chart. ' +
      'The last known points are still shown — they are not faked ' +
      'or extrapolated. Judge miner health by accepted shares, not ' +
      'by a chart that stopped updating.';
    if (!staleTooltipTerm) return STALE_FALLBACK;
    const resolved = glossaryText(staleTooltipTerm);
    return resolved || STALE_FALLBACK;
  }, [staleTooltipTerm]);

  // Empty state — no series at all, OR series present but every value is
  // non-finite (all-NaN). Both render the same honest centered placeholder
  // over a faint grid hint so the panel never looks broken or blank-framed.
  if (activeSeries.length === 0 || !hasFiniteValue) {
    return (
      <div ref={containerRef} className={className} style={{ width: '100%', ...style }}>
        <svg
          viewBox={`0 0 ${chartWidth} ${chartHeight}`}
          width="100%"
          height={height}
          preserveAspectRatio="xMidYMid meet"
          {...ariaProps}
          style={{ display: 'block' }}
        >
          {/* Faint placeholder grid so the panel doesn't look broken. */}
          {[0.25, 0.5, 0.75].map((frac, i) => {
            const y = padding.top + plotHeight * frac;
            return (
              <line
                key={`empty-grid-${i}`}
                x1={padding.left} y1={y}
                x2={chartWidth - padding.right} y2={y}
                stroke="rgba(255,255,255,0.04)"
                strokeWidth={1}
                strokeDasharray="2 4"
                shapeRendering="crispEdges"
              />
            );
          })}
          <text
            x={chartWidth / 2}
            y={chartHeight / 2 + 4}
            textAnchor="middle"
            fill="var(--text-dim, #6E6E80)"
            fontSize="12"
            fontFamily="var(--font-mono, 'JetBrains Mono', monospace)"
            letterSpacing="0.08em"
          >
            {emptyText.toUpperCase()}
          </text>
        </svg>
      </div>
    );
  }

  return (
    <div ref={containerRef} className={className} style={{ width: '100%', position: 'relative', ...style }}>
      <svg
        viewBox={`0 0 ${chartWidth} ${chartHeight}`}
        width="100%"
        height={height}
        preserveAspectRatio="xMidYMid meet"
        {...ariaProps}
        onMouseMove={handleMouseMove}
        onMouseLeave={handleMouseLeave}
        style={{ display: 'block' }}
      >
        <defs>
          {activeSeries.map((s, i) => (
            <linearGradient
              key={`grad${i}`}
              id={`svgchart-grad-${uid}-${i}`}
              x1="0" y1="0" x2="0" y2="1"
            >
              <stop offset="0%" stopColor={s.color} stopOpacity="0.34" />
              <stop offset="38%" stopColor={s.color} stopOpacity="0.15" />
              <stop offset="72%" stopColor={s.color} stopOpacity="0.045" />
              <stop offset="100%" stopColor={s.color} stopOpacity="0" />
            </linearGradient>
          ))}
          {/* Soft phosphor glow under the primary stroke — premium, cheap
              (single blur), and capped low so dense overlays stay readable. */}
          <filter
            id={`svgchart-glow-${uid}`}
            x="-12%" y="-12%" width="124%" height="124%"
          >
            <feGaussianBlur stdDeviation="2.1" result="b" />
            <feMerge>
              <feMergeNode in="b" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
        </defs>

        {gridElements}
        {yAxisLabels}
        {xAxisLabels}
        {seriesElements}

        {hoverInfo && (
          <line
            x1={hoverInfo.svgX}
            y1={padding.top}
            x2={hoverInfo.svgX}
            y2={padding.top + plotHeight}
            stroke="rgba(255,255,255,0.18)"
            strokeWidth={1}
            strokeDasharray="2 3"
          />
        )}
        {hoverInfo && tooltipData?.values.map((v, i) => (
          v.visible ? (
            <g key={`marker${i}`} pointerEvents="none">
              <circle cx={v.pointX} cy={v.pointY} r={4.6} fill={v.color} opacity={0.18} />
              <circle cx={v.pointX} cy={v.pointY} r={2.4} fill={v.color} />
            </g>
          ) : null
        ))}
      </svg>

      {/* Stale-data badge — TRUTH CONTRACT: staleness is never visually
          suppressed. The badge renders whenever `isStale`, unconditionally;
          `data-tooltip` only ADDS an honest explanation (F6 `telemetry_stale`
          key by default, verbatim Wave-5 string as a hard fallback) and never
          hides or softens the badge. `pointer-events:auto` (set in CSS) so
          the explainer is reachable on hover/focus. */}
      {isStale && (
        <span
          className="svgchart-stale-badge"
          tabIndex={0}
          role="status"
          aria-label="Telemetry stale — no recent samples"
          data-tooltip={staleTooltipText}
          data-tooltip-pos="bottom"
        >
          Stale
        </span>
      )}

      {hoverInfo && tooltipData && (
        <div
          className="svgchart-readout"
          role="tooltip"
          style={{
            left: Math.min(hoverInfo.mouseX + 12, (containerRef.current?.clientWidth ?? 300) - 150),
            top: Math.max(0, hoverInfo.mouseY - 40),
          }}
        >
          <div className="svgchart-readout-time">{tooltipData.time}</div>
          {tooltipData.values.map((v, i) => (
            <div key={i} className="svgchart-readout-row" style={{ color: v.color }}>
              <span className="svgchart-readout-label">{v.label}</span>
              <strong className="svgchart-readout-val">{v.value}</strong>
            </div>
          ))}
        </div>
      )}
    </div>
  );
});
