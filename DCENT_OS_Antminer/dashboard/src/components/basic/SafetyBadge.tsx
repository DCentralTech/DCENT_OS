import React from 'react';
import { useMinerStore } from '../../store/miner';
import { glossaryText } from '../../utils/glossary';

type SafetyLevel = 'normal' | 'warm' | 'hot';

function getSafetyLevel(chipTempC: number | null): SafetyLevel {
  if (chipTempC == null || chipTempC < 60) return 'normal';
  if (chipTempC <= 65) return 'warm';
  return 'hot';
}

// Labels and level class only — colors come entirely from CSS tokens
// (--green / --yellow / --red defined on .mode-basic).
// HEATER-7: these are OBSERVATIONS of the chip temperature, not assertions of
// a throttle action. The dashboard has no real backend throttle/safety-action
// flag here (only raw chip temp), so "hot" reports "Running hot" rather than
// claiming "power reduced" — a state we can't verify happened.
const SAFETY_CONFIG: Record<SafetyLevel, { label: string }> = {
  normal: { label: 'All systems normal' },
  warm:   { label: 'Running warm' },
  hot:    { label: 'Running hot' },
};

function ShieldIcon({ level, size = 18 }: { level: SafetyLevel; size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
      {/* State-specific inner symbol for colorblind accessibility */}
      {level === 'normal' && (
        <polyline points="9 12 11 14 15 10" />
      )}
      {level === 'warm' && (
        <>
          <line x1="12" y1="9" x2="12" y2="13" />
          <line x1="12" y1="15" x2="12" y2="15.5" />
        </>
      )}
      {level === 'hot' && (
        <>
          <line x1="10" y1="10" x2="14" y2="14" />
          <line x1="14" y1="10" x2="10" y2="14" />
        </>
      )}
    </svg>
  );
}

export function SafetyBadge() {
  const status = useMinerStore(s => s.status);

  // Get max chip temp across all chains
  const chipTemp = status?.chains && status.chains.length > 0
    ? Math.max(...status.chains.map(c => c.temp_c))
    : null;

  const level = getSafetyLevel(chipTemp);
  const config = SAFETY_CONFIG[level];

  const safetyTip =
    level === 'normal'
      ? glossaryText('cut_hash_before_noise')
      : level === 'warm'
        ? 'The chips are a little warm. DCENT_OS is designed to ease power down before asking for more fan. Noise still needs tach/RPM proof.'
        : 'The chips are running hot. DCENT_OS is designed to cut hash power before raising fan noise, and auto-shutoff is always active — keep an eye on temperatures.';

  return (
    <div
      className={`safety-badge safety-badge--${level}`}
      role="status"
      aria-label={`Thermal safety: ${config.label}`}
      aria-live="polite"
      data-tooltip={safetyTip}
    >
      <ShieldIcon level={level} />
      <div className="safety-badge__body">
        <div className="safety-badge__label">{config.label}</div>
        <div className="safety-badge__sub">Auto-shutoff is always active</div>
      </div>
    </div>
  );
}
