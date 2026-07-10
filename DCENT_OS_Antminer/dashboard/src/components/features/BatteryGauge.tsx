import React from 'react';

interface BatteryGaugeProps {
  soc_pct: number;
  voltage_v: number;
  current_a: number;
  zone: string;
}

const ZONE_COLORS: Record<string, string> = {
  critical: 'var(--red)',
  low: 'var(--yellow)',
  normal: 'var(--green)',
  high: '#60A5FA',
  full: '#60A5FA',
};

export function BatteryGauge({ soc_pct, voltage_v, current_a, zone }: BatteryGaugeProps) {
  const color = ZONE_COLORS[zone] || 'var(--green)';
  const fillH = Math.max(2, (soc_pct / 100) * 120);
  const charging = current_a < -0.1;
  const isCritical = zone === 'critical';

  return (
    <div style={{ textAlign: 'center' }}>
      <svg viewBox="0 0 60 160" width="60" height="160">
        {/* Terminal cap */}
        <rect x="20" y="2" width="20" height="8" rx="3" fill="var(--border)" />
        {/* Battery body */}
        <rect x="10" y="10" width="40" height="130" rx="5"
          fill="none" stroke="var(--border)" strokeWidth="2" />
        {/* Fill level */}
        <rect x="13" y={140 - fillH} width="34" height={fillH} rx="3"
          fill={color} opacity={0.85}
          style={isCritical ? { animation: 'batteryPulse 1.5s ease-in-out infinite' } : undefined}
        />
        {/* SoC text */}
        <text x="30" y="80" textAnchor="middle" dominantBaseline="middle"
          fill="var(--text)" fontFamily="'JetBrains Mono', monospace"
          fontWeight="700" fontSize="14">
          {Math.round(soc_pct)}%
        </text>
        {/* Charging bolt */}
        {charging && (
          <path d="M28 50 L24 68 L30 66 L26 82 L36 60 L30 62 L34 50Z"
            fill="var(--accent)" opacity={0.9} />
        )}
      </svg>
      {/* Voltage */}
      <div style={{
        fontFamily: "'JetBrains Mono', monospace", fontWeight: 700,
        fontSize: '1rem', color, marginTop: 4,
      }}>
        {voltage_v.toFixed(1)}V
      </div>
      {/* Current */}
        <div style={{
          fontFamily: "'JetBrains Mono', monospace",
          fontSize: '0.75rem', color: charging ? 'var(--green)' : 'var(--text-dim)',
          marginTop: 2,
        }}>
          {charging ? '+' : ''}{current_a.toFixed(1)}A
        </div>
      </div>
  );
}
