import React from 'react';

interface PowerFlowDiagramProps {
  solar_watts: number;
  battery_watts: number;
  miner_watts: number;
  battery_soc_pct: number;
  charging: boolean;
  zone: string;
}

const ZONE_COLORS: Record<string, string> = {
  critical: 'var(--red)',
  low: 'var(--yellow)',
  normal: 'var(--green)',
  high: '#60A5FA',
  full: '#60A5FA',
};

export function PowerFlowDiagram({
  solar_watts, battery_watts, miner_watts, battery_soc_pct, charging, zone,
}: PowerFlowDiagramProps) {
  const batteryColor = ZONE_COLORS[zone] || 'var(--green)';

  return (
    <div style={{ padding: 8 }}>
      <svg viewBox="0 0 420 120" width="100%" height="120"
        style={{ maxWidth: 420, margin: '0 auto', display: 'block' }}>
        <defs>
          <style>{`
            @keyframes flowArrow { to { stroke-dashoffset: -12; } }
            .flow-arrow { stroke-dasharray: 8 4; animation: flowArrow 1s linear infinite; }
            .flow-arrow-rev { stroke-dasharray: 8 4; animation: flowArrow 1s linear infinite reverse; }
          `}</style>
        </defs>

        {/* Solar node */}
        <g transform="translate(30,30)">
          <rect x="-25" y="-20" width="50" height="40" rx="6"
            fill="none" stroke={solar_watts > 0 ? 'var(--green)' : 'var(--border)'}
            strokeWidth="2" />
          <text x="0" y="-2" textAnchor="middle" fill="var(--text)"
            fontSize="10" fontWeight="600">Solar</text>
          <text x="0" y="12" textAnchor="middle"
            fill={solar_watts > 0 ? 'var(--green)' : 'var(--text-dim)'}
            fontSize="9" fontFamily="'JetBrains Mono', monospace">
            {solar_watts > 0 ? `${solar_watts}W` : 'N/A'}
          </text>
        </g>

        {/* Battery node */}
        <g transform="translate(195,30)">
          <rect x="-30" y="-25" width="60" height="50" rx="6"
            fill="none" stroke={batteryColor} strokeWidth="2" />
          {/* Mini battery fill */}
          <rect x="-24" y={25 - (battery_soc_pct / 100 * 44)} width="48"
            height={battery_soc_pct / 100 * 44} rx="3"
            fill={batteryColor} opacity="0.3" />
          <text x="0" y="-5" textAnchor="middle" fill="var(--text)"
            fontSize="10" fontWeight="600">Battery</text>
          <text x="0" y="8" textAnchor="middle" fill={batteryColor}
            fontSize="11" fontFamily="'JetBrains Mono', monospace" fontWeight="700">
            {Math.round(battery_soc_pct)}%
          </text>
          <text x="0" y="40" textAnchor="middle" fill="var(--text-dim)"
            fontSize="8" fontFamily="'JetBrains Mono', monospace">
            {battery_watts > 0 ? `${battery_watts}W` : ''}
          </text>
        </g>

        {/* Miner node */}
        <g transform="translate(370,30)">
          <rect x="-30" y="-20" width="60" height="40" rx="6"
            fill="none" stroke={miner_watts > 0 ? 'var(--accent)' : 'var(--border)'}
            strokeWidth="2" />
          <text x="0" y="-2" textAnchor="middle" fill="var(--text)"
            fontSize="10" fontWeight="600">Miner</text>
          <text x="0" y="12" textAnchor="middle"
            fill={miner_watts > 0 ? 'var(--accent)' : 'var(--text-dim)'}
            fontSize="9" fontFamily="'JetBrains Mono', monospace">
            {miner_watts > 0 ? `${miner_watts}W` : 'Idle'}
          </text>
        </g>

        {/* Arrow: Solar → Battery */}
        {solar_watts > 0 && (
          <g>
            <line x1="60" y1="30" x2="160" y2="30"
              stroke="var(--green)" strokeWidth={Math.min(4, 2 + solar_watts / 500)}
              className="flow-arrow" />
            <text x="110" y="22" textAnchor="middle" fill="var(--green)"
              fontSize="8" fontFamily="'JetBrains Mono', monospace">
              {solar_watts}W
            </text>
          </g>
        )}

        {/* Arrow: Battery → Miner */}
        {miner_watts > 0 && (
          <g>
            <line x1="230" y1="30" x2="335" y2="30"
              stroke="var(--accent)" strokeWidth={Math.min(4, 2 + miner_watts / 500)}
              className={charging ? 'flow-arrow' : 'flow-arrow'} />
            <text x="282" y="22" textAnchor="middle" fill="var(--accent)"
              fontSize="8" fontFamily="'JetBrains Mono', monospace">
              {miner_watts}W
            </text>
          </g>
        )}

        {/* Inactive arrows (dim) */}
        {solar_watts === 0 && (
          <line x1="60" y1="30" x2="160" y2="30"
            stroke="var(--border)" strokeWidth="1" strokeDasharray="4 4" />
        )}
        {miner_watts === 0 && (
          <line x1="230" y1="30" x2="335" y2="30"
            stroke="var(--border)" strokeWidth="1" strokeDasharray="4 4" />
        )}

        {/* Net flow label */}
        <text x="210" y="100" textAnchor="middle" fill="var(--text-dim)"
          fontSize="9" fontFamily="'JetBrains Mono', monospace">
          {charging ? `Net: +${(solar_watts - miner_watts).toFixed(0)}W (charging)` :
            miner_watts > 0 ? `Net: -${miner_watts}W (discharging)` : 'Idle'}
        </text>
      </svg>
    </div>
  );
}
