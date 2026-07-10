// KitFanMonitor — structural recreation of the design-kit's
// `DashboardPage.jsx` FanMonitor section (the compact dashboard variant,
// NOT the full TempFansPage FanMonitor).
//
// Kit reference: ui_kits/dashboard/DashboardPage.jsx (FanMonitor + FanGauge):
//   <div className="section">
//     <div className="section-title">Fan Monitor <a className="right">Configure fans</a></div>
//     grid of 4 circular FanGauge (svg ring + RPM center + FANn label)
//     footer line: Average speed · Mode
//   </div>
//
// Every gauge is fed from REAL `status.fans` (per-fan RPM + PWM). When the
// miner reports no per-fan data we fall back to the single aggregate fan
// reading; when there is no fan telemetry at all we render an honest
// "no fan telemetry" state rather than a fabricated gauge.
import React from 'react';
import { useMinerStore } from '../../store/miner';
import type { PerFanReading } from '../../api/types';

// Mirrors the kit's FanGauge: an 80px SVG with a background ring + a colored
// progress arc (green/yellow/red by PWM), RPM in the center, FANn caption.
function FanGauge({ rpm, pct, id }: { rpm: number; pct: number; id: number }) {
  const r = 34;
  const c = 2 * Math.PI * r;
  const dash = (Math.min(pct, 99.9) / 100) * c;
  const color = pct > 85 ? 'var(--red)' : pct > 60 ? 'var(--yellow)' : 'var(--accent)';
  return (
    <div style={{ textAlign: 'center', position: 'relative', width: 90 }}>
      <svg width="80" height="80" viewBox="0 0 80 80">
        <circle cx="40" cy="40" r={r} fill="none" stroke="rgba(255,255,255,.08)" strokeWidth="5" />
        {pct > 0 && (
          <circle
            cx="40"
            cy="40"
            r={r}
            fill="none"
            stroke={color}
            strokeWidth="5"
            strokeLinecap="round"
            strokeDasharray={`${dash} ${c}`}
            transform="rotate(-90 40 40)"
            style={{
              filter: 'drop-shadow(0 0 4px rgba(250, 165, 0,.4))',
              transition: 'stroke-dasharray .5s',
            }}
          />
        )}
      </svg>
      <div
        style={{
          position: 'absolute',
          inset: 0,
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          flexDirection: 'column',
          pointerEvents: 'none',
          height: 80,
        }}
      >
        <div
          style={{
            fontFamily: 'var(--font-mono)',
            fontWeight: 700,
            fontSize: '.88rem',
            color: 'var(--fg-primary)',
          }}
        >
          {rpm > 0 ? rpm.toLocaleString() : '---'}
        </div>
        <div style={{ fontSize: '.6rem', color: 'var(--fg-dim)', letterSpacing: '.05em' }}>RPM</div>
      </div>
      <div
        style={{
          fontSize: '.65rem',
          color: 'var(--fg-dim)',
          fontWeight: 700,
          letterSpacing: '.08em',
          marginTop: 4,
        }}
      >
        FAN{id}
      </div>
    </div>
  );
}

export function KitFanMonitor({ onConfigure }: { onConfigure: () => void }) {
  const fans = useMinerStore(s => s.status?.fans);
  const pwm = fans?.pwm ?? 0;
  const rpm = fans?.rpm ?? 0;

  const perFan: PerFanReading[] = (fans?.per_fan && fans.per_fan.length > 0)
    ? fans.per_fan
    : rpm > 0 || pwm > 0
      ? [{ id: 0, rpm, pwm_percent: Math.round(pwm) }]
      : [];

  const avgPct = perFan.length > 0
    ? Math.round(perFan.reduce((s, f) => s + f.pwm_percent, 0) / perFan.length)
    : 0;

  return (
    <div className="section" style={{ marginBottom: 0 }} data-testid="kit-fan-monitor">
      <div className="section-title">
        Fan Monitor
        <button
          type="button"
          className="right section-link-btn"
          onClick={onConfigure}
          data-tip="Open the cooling editor and manual fan override."
        >
          Configure fans
        </button>
      </div>
      {perFan.length > 0 ? (
        <>
          <div
            style={{
              display: 'grid',
              gridTemplateColumns: `repeat(${Math.min(4, Math.max(1, perFan.length))}, 1fr)`,
              gap: 8,
              padding: '8px 0',
              placeItems: 'center',
            }}
          >
            {perFan.map(f => (
              <FanGauge key={f.id} rpm={f.rpm} pct={f.pwm_percent} id={f.id} />
            ))}
          </div>
          <div
            style={{
              display: 'flex',
              justifyContent: 'center',
              gap: 24,
              marginTop: 10,
              fontSize: '.78rem',
            }}
          >
            <span style={{ color: 'var(--fg-secondary)' }}>
              Average speed
              <strong style={{ color: 'var(--fg-primary)', marginLeft: 4 }}>{avgPct} %</strong>
            </span>
            {/* Wave-13: was a hardcoded "Mode: Automatic" — FanState carries no
                mode field, so that was an unverifiable claim. Show tach
                confirmation instead (derived from real per-fan RPM). */}
            <span style={{ color: 'var(--fg-secondary)' }}>
              Tach
              <strong style={{ color: 'var(--fg-primary)', marginLeft: 4 }}>
                {perFan.some(f => f.rpm > 0) ? 'confirmed' : 'no signal'}
              </strong>
            </span>
          </div>
        </>
      ) : (
        <div
          style={{
            padding: '28px 12px',
            textAlign: 'center',
            color: 'var(--fg-dim)',
            fontSize: '.85rem',
          }}
        >
          No fan telemetry reported yet.
        </div>
      )}
    </div>
  );
}
