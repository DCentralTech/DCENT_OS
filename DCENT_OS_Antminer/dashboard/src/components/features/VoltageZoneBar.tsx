import React from 'react';

interface VoltageZoneBarProps {
  voltage_v: number;
  critical_v: number;
  low_v: number;
  high_v: number;
  full_v: number;
}

const ZONE_DEFS = [
  { key: 'critical', color: 'var(--red)', label: 'CRIT' },
  { key: 'low', color: 'var(--yellow)', label: 'LOW' },
  { key: 'normal', color: 'var(--green)', label: 'NORMAL' },
  { key: 'high', color: '#60A5FA', label: 'HIGH' },
  { key: 'full', color: '#93C5FD', label: 'FULL' },
];

export function VoltageZoneBar({ voltage_v, critical_v, low_v, high_v, full_v }: VoltageZoneBarProps) {
  const vMin = critical_v - 2;
  const vMax = full_v + 2;
  const range = vMax - vMin;
  if (range <= 0) return null;

  const pct = (v: number) => ((v - vMin) / range * 100).toFixed(1);
  const markerPct = Math.max(0, Math.min(100, (voltage_v - vMin) / range * 100));

  // Zone boundaries as percentages
  const boundaries = [vMin, critical_v, low_v, high_v, full_v, vMax];
  const zones = ZONE_DEFS.map((def, i) => ({
    ...def,
    left: parseFloat(pct(boundaries[i])),
    width: parseFloat(pct(boundaries[i + 1])) - parseFloat(pct(boundaries[i])),
  }));

  // Current zone color
  const currentColor = voltage_v < critical_v ? 'var(--red)'
    : voltage_v < low_v ? 'var(--yellow)'
    : voltage_v <= high_v ? 'var(--green)'
    : voltage_v <= full_v ? '#60A5FA'
    : '#93C5FD';

  return (
    <div style={{ padding: '8px 0' }}>
      {/* Voltage readout above bar */}
      <div style={{
        position: 'relative', height: 24, marginBottom: 4,
      }}>
        <div style={{
          position: 'absolute',
          left: `${markerPct}%`,
          transform: 'translateX(-50%)',
          fontFamily: "'JetBrains Mono', monospace",
          fontWeight: 700, fontSize: '0.85rem',
          color: currentColor,
          transition: 'left 0.5s ease',
        }}>
          {voltage_v.toFixed(1)}V
        </div>
      </div>

      {/* Zone bar */}
      <div style={{
        position: 'relative', height: 28,
        borderRadius: 6, overflow: 'hidden',
        display: 'flex',
      }}>
        {zones.map(z => (
          <div key={z.key} style={{
            flex: `${z.width} 0 0`,
            background: z.color,
            opacity: 0.7,
            borderRight: '1px solid var(--bg)',
          }} />
        ))}

        {/* Marker triangle */}
        <div style={{
          position: 'absolute',
          left: `${markerPct}%`,
          top: -6,
          transform: 'translateX(-50%)',
          width: 0, height: 0,
          borderLeft: '6px solid transparent',
          borderRight: '6px solid transparent',
          borderTop: '8px solid var(--text)',
          transition: 'left 0.5s ease',
        }} />
      </div>

      {/* Zone labels */}
      <div style={{
        position: 'relative', height: 16, marginTop: 4,
      }}>
        {zones.map(z => (
          <div key={z.key} style={{
            position: 'absolute',
            left: `${z.left + z.width / 2}%`,
            transform: 'translateX(-50%)',
            fontSize: '0.55rem',
            color: 'var(--text-dim)',
            fontFamily: "'JetBrains Mono', monospace",
          }}>
            {z.label}
          </div>
        ))}
      </div>

      {/* Threshold values */}
      <div style={{
        display: 'flex', justifyContent: 'space-between',
        fontSize: '0.6rem', color: 'var(--text-dim)',
        fontFamily: "'JetBrains Mono', monospace",
        marginTop: 2,
      }}>
        <span>{critical_v}V</span>
        <span>{low_v}V</span>
        <span>{high_v}V</span>
        <span>{full_v}V</span>
      </div>
    </div>
  );
}
