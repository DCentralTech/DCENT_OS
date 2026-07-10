import React, { useMemo } from 'react';
import { useMinerStore } from '../../store/miner';
import type { HistoryPoint } from '../../api/types';

/**
 * HeaterRoomTempGraph — recreation of the Claude-Design Heater kit
 * `RoomTempGraph` card LOOK (HeaterMode.jsx:311 `.nest-graph`), but fed by
 * the REAL miner history the parent (HistoryView) already fetches via
 * `api.getHeaterHistory()`.
 *
 * TRUTH CONTRACT: the kit prototype fabricated a 96-point sine "room temp"
 * series + dotted "target" line. Production has NO ambient room-temp probe
 * and NO heater target on this surface — the only honest temperature signal
 * is the miner's reported `temp_c` per `HistoryPoint`. So this renders the
 * REAL chip/board temperature trend over the loaded window and labels it
 * exactly that. No fabricated room series, no fake target line. If there is
 * no real series yet, the kit card chrome renders with an honest empty state.
 */

interface HeaterRoomTempGraphProps {
  history: HistoryPoint[];
  /** Loading flag forwarded from the parent's existing fetch lifecycle. */
  loading?: boolean;
}

const W = 320;
const H = 100;
const P = 14;

export function HeaterRoomTempGraph({ history, loading = false }: HeaterRoomTempGraphProps) {
  const tempUnit = useMinerStore(s => s.settings.temperatureUnit);

  // Real temperature series — chronological, finite samples only. No
  // synthesised points; we only ever draw what the miner actually reported.
  const series = useMemo(() => {
    return history
      .filter(p => typeof p.temp_c === 'number' && isFinite(p.temp_c))
      .map(p => ({
        t: (p.timestamp_s ?? p.timestamp) * 1000,
        c: p.temp_c,
      }))
      .sort((a, b) => a.t - b.t);
  }, [history]);

  const unitLabel = tempUnit === 'F' ? '°F' : '°C';

  const hasSeries = series.length >= 2;

  const chart = useMemo(() => {
    if (!hasSeries) return null;
    const conv = (c: number) => (tempUnit === 'F' ? (c * 9) / 5 + 32 : c);
    const vals = series.map(s => conv(s.c));
    const lo = Math.min(...vals);
    const hi = Math.max(...vals);
    // Pad the band so a flat trend is not a hairline on the floor.
    const pad = Math.max(1, (hi - lo) * 0.15);
    const minY = lo - pad;
    const maxY = hi + pad;
    const span = maxY - minY || 1;
    const x = (i: number) => P + (i / (series.length - 1)) * (W - 2 * P);
    const y = (v: number) => H - P - ((v - minY) / span) * (H - 2 * P);
    const path = vals
      .map((v, i) => `${i === 0 ? 'M' : 'L'}${x(i).toFixed(1)} ${y(v).toFixed(1)}`)
      .join(' ');
    const area = `${path} L ${x(series.length - 1).toFixed(1)} ${H - P} L ${x(0).toFixed(1)} ${H - P} Z`;
    return {
      path,
      area,
      lastX: x(series.length - 1),
      lastY: y(vals[vals.length - 1]),
      lo,
      hi,
      latest: vals[vals.length - 1],
    };
  }, [series, hasSeries, tempUnit]);

  const fmtTime = (ms: number) =>
    new Date(ms).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });

  return (
    <div
      className="nest-card nest-graph heater-room-temp-graph"
      data-testid="heater-room-temp-graph"
      data-tooltip="Real temperature the miner reported over the loaded history window. This firmware has no separate room-temperature probe, so this is the miner's own chip/board sensor — not a fabricated room curve."
    >
      <div className="nest-card-eyebrow heater-room-temp-graph-eyebrow">
        <svg
          viewBox="0 0 24 24"
          width="20"
          height="20"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.6"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <line x1="18" y1="20" x2="18" y2="10" />
          <line x1="12" y1="20" x2="12" y2="4" />
          <line x1="6" y1="20" x2="6" y2="14" />
        </svg>
        Miner temperature
        {chart && (
          <span className="nest-card-meta heater-room-temp-graph-meta">
            now {chart.latest.toFixed(1)}{unitLabel} · range {chart.lo.toFixed(1)}–{chart.hi.toFixed(1)}{unitLabel}
          </span>
        )}
      </div>

      {loading ? (
        <div className="heater-room-temp-graph-empty" aria-live="polite">
          Loading temperature history…
        </div>
      ) : !hasSeries || !chart ? (
        <div className="heater-room-temp-graph-empty" data-testid="heater-room-temp-graph-empty">
          <div>No history yet</div>
          <div className="heater-room-temp-graph-empty-hint">
            Once the miner has been running, its temperature trend appears here.
          </div>
        </div>
      ) : (
        <>
          <svg
            viewBox={`0 0 ${W} ${H}`}
            width="100%"
            preserveAspectRatio="xMidYMid meet"
            role="img"
            aria-label={`Miner temperature trend over ${series.length} samples — currently ${chart.latest.toFixed(1)} ${unitLabel}, ranging ${chart.lo.toFixed(1)} to ${chart.hi.toFixed(1)} ${unitLabel}`}
          >
            <defs>
              <linearGradient id="hrtg-fill" x1="0" x2="0" y1="0" y2="1">
                <stop offset="0%" stopColor="#FAA500" stopOpacity=".24" />
                <stop offset="100%" stopColor="#FAA500" stopOpacity="0" />
              </linearGradient>
            </defs>
            <path d={chart.area} fill="url(#hrtg-fill)" />
            <path
              d={chart.path}
              fill="none"
              stroke="#FAA500"
              strokeWidth="1.8"
              vectorEffect="non-scaling-stroke"
              style={{ filter: 'drop-shadow(0 0 4px rgba(250,165,0,.4))' }}
            />
            <circle
              cx={chart.lastX}
              cy={chart.lastY}
              r="3.5"
              fill="#FAA500"
              stroke="#1a0f00"
              strokeWidth="1.5"
            />
          </svg>
          <div className="nest-graph-axis heater-room-temp-graph-axis">
            <span>{fmtTime(series[0].t)}</span>
            <span>{fmtTime(series[Math.floor(series.length / 2)].t)}</span>
            <span>now</span>
          </div>
        </>
      )}
    </div>
  );
}
