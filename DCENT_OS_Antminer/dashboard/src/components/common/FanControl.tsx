import React, { useEffect, useRef, useState } from 'react';
import { glossaryText } from '../../utils/glossary';

/**
 * Wire-level fan mode id. Kept as the OS legacy set so the `useFanControl`
 * hook's `Record<FanMode, …>` (and the `/api/fan` contract it feeds) stay
 * exhaustive and unchanged — this is the value handed to `onModeChange`.
 */
export type FanMode = 'quiet' | 'balanced' | 'performance' | 'custom';

/**
 * Canonical cross-firmware FanMode ids (DCENT Design Language —
 * component-contract.md §7 COMP-FAN): `quiet | managed | override | custom`.
 * The OS legacy ids (`balanced`/`performance`) are pure aliases of `managed`/
 * `override`; this map documents the 1:1 correspondence WITHOUT renaming the
 * wire id (the display-string reconciliation is owned by the terminology
 * contract). The canonical id rides on a `data-mode-canonical` attribute for
 * SR/test/future-CSS, exactly like StatusPill's `data-state` — additive, no
 * safety behavior touched.
 */
export type FanModeCanonical = 'quiet' | 'managed' | 'override' | 'custom';

export const CANONICAL_FAN_MODE: Record<FanMode, FanModeCanonical> = {
  quiet: 'quiet',
  balanced: 'managed',
  performance: 'override',
  custom: 'custom',
};

interface FanControlProps {
  currentPwm: number;
  currentRpm: number;
  activeMode?: FanMode | null;
  modeSource?: 'daemon' | 'unknown';
  onModeChange?: (mode: FanMode) => void;
  onPwmChange?: (pwm: number) => void;
  disabled?: boolean;
}

const FAN_MODES: { id: FanMode; canonical: FanModeCanonical; name: string; desc: string; pwmRange: string }[] = [
  { id: 'quiet', canonical: 'quiet', name: 'Home Idle', desc: 'Low request', pwmRange: '10' },
  { id: 'balanced', canonical: 'managed', name: 'Managed', desc: 'Daemon PID', pwmRange: 'Auto' },
  { id: 'performance', canonical: 'override', name: 'Cooling Override', desc: 'Clamped by daemon', pwmRange: 'Guarded' },
  { id: 'custom', canonical: 'custom', name: 'Home Cap', desc: 'Manual cap', pwmRange: '10-30' },
];

// Home fan-cap posture: the dashboard only exposes 10-30 for manual requests.
// Loud airflow belongs to daemon thermal overrides with RPM proof.
// Exported so the home-quiet clamp is regression-pinnable (FanControl.test.tsx).
export function pwmZone(pwm: number): { zone: 'home' | 'override' | 'emergency'; label: string } {
  if (pwm <= 30) return { zone: 'home', label: 'Home cap' };
  if (pwm <= 60) return { zone: 'override', label: 'Loud override' };
  return { zone: 'emergency', label: 'Thermal override' };
}

export function clampHomePwm(pwm: number): number {
  return Math.max(10, Math.min(30, Number.isFinite(pwm) && pwm > 0 ? pwm : 10));
}

export function normalizeFanMode(mode: string | null | undefined): FanMode | null {
  const normalized = mode?.trim().toLowerCase().replace(/[\s_-]+/g, '');
  switch (normalized) {
    case 'quiet':
    case 'home':
    case 'homeidle':
      return 'quiet';
    case 'auto':
    case 'balanced':
    case 'managed':
    case 'daemonpid':
      return 'balanced';
    case 'performance':
    case 'override':
    case 'coolingoverride':
    case 'fullrange':
      return 'performance';
    case 'custom':
    case 'manual':
    case 'homecap':
      return 'custom';
    default:
      return null;
  }
}

export function FanControl({
  currentPwm,
  currentRpm,
  activeMode,
  modeSource = 'unknown',
  onModeChange,
  onPwmChange,
  disabled,
}: FanControlProps) {
  const [pendingMode, setPendingMode] = useState<FanMode | null>(null);
  const [customPwm, setCustomPwm] = useState(clampHomePwm(currentPwm));
  const customPwmRef = useRef(clampHomePwm(currentPwm));
  const wasDisabledRef = useRef(Boolean(disabled));

  const confirmedMode = modeSource === 'daemon' ? activeMode ?? null : null;
  const lastConfirmedModeRef = useRef<FanMode | null>(confirmedMode);

  useEffect(() => {
    const confirmedChanged = modeSource === 'daemon' && confirmedMode !== lastConfirmedModeRef.current;
    if (!pendingMode) {
      lastConfirmedModeRef.current = confirmedMode;
      wasDisabledRef.current = Boolean(disabled);
      return;
    }
    if (confirmedChanged) {
      setPendingMode(null);
    } else if (wasDisabledRef.current && !disabled) {
      setPendingMode(null);
    }
    lastConfirmedModeRef.current = confirmedMode;
    wasDisabledRef.current = Boolean(disabled);
  }, [confirmedMode, disabled, modeSource, pendingMode]);

  const handleModeClick = (mode: FanMode) => {
    setPendingMode(mode);
    onModeChange?.(mode);
  };

  const zone = pwmZone(customPwm);
  const showCustomSlider = confirmedMode === 'custom' || pendingMode === 'custom';
  const pendingLabel = pendingMode ? FAN_MODES.find(m => m.id === pendingMode)?.name ?? pendingMode : null;

  return (
    <div>
      <div className="fan-modes">
        {FAN_MODES.map(m => {
          const isActive = confirmedMode === m.id;
          const isPending = pendingMode === m.id;
          return (
            <button
              key={m.id}
              className={`fan-mode-btn${isActive ? ' active' : ''}${isPending ? ' is-applying' : ''}`}
              onClick={() => handleModeClick(m.id)}
              disabled={disabled}
              data-mode-canonical={m.canonical}
              data-mode-source={modeSource}
              aria-pressed={isActive}
            >
              <span className="mode-name">{m.name}</span>
              <span className="mode-desc">{isPending ? 'Applying...' : m.desc}</span>
            </button>
          );
        })}
      </div>
      <div className="cp-fan-mode-hint" role="status">
        {pendingLabel
          ? `Applying ${pendingLabel}; waiting for daemon telemetry.`
          : confirmedMode
            ? 'Fan mode reported by daemon.'
            : 'Fan mode not reported by daemon; no preset is highlighted.'}
      </div>

      <div className="cp-fan-readouts">
        <div
          className="cp-fan-readout"
          data-tooltip={glossaryText('fan_pwm')}
        >
          <div className="cp-fan-readout-label">PWM</div>
          <div className="cp-fan-readout-value">{currentPwm}</div>
        </div>
        <div className="cp-fan-readout">
          <div className="cp-fan-readout-label">RPM</div>
          <div className={`cp-fan-readout-value${currentRpm > 0 ? '' : ' is-stopped'}`}>
            {currentRpm > 0 ? currentRpm.toLocaleString() : 'No tach'}
          </div>
        </div>
        <div
          className="cp-fan-readout"
          data-tooltip={glossaryText('cut_hash_before_noise')}
        >
          <div className="cp-fan-readout-label">Speed</div>
          <div className="cp-fan-readout-value">{currentRpm > 0 ? 'RPM' : 'Unproved'}</div>
        </div>
      </div>

      {showCustomSlider && (
        <div className="cp-fan-slider-wrap">
          {/* Zone gradient track sits visually behind the transparent native range. */}
          <div className="cp-fan-slider-zones" aria-hidden="true" />
          <input
            type="range" min="10" max="30" value={customPwm}
            className="cp-fan-slider"
            onChange={(e) => {
              const v = Number(e.target.value);
              setCustomPwm(v);
              customPwmRef.current = v;
            }}
            onMouseUp={() => onPwmChange?.(customPwmRef.current)}
            onTouchEnd={() => onPwmChange?.(customPwmRef.current)}
            // Keyboard-only commit: arrow-key edits update customPwm/customPwmRef
            // via onChange but, like a drag, only reach the daemon on release.
            // Commit on keyUp with the SAME ref the mouse/touch paths use so an
            // arrow-key change is debounced + sent by useFanControl identically.
            onKeyUp={() => onPwmChange?.(customPwmRef.current)}
            aria-label="Home fan PWM cap, 10 to 30"
            aria-valuetext={`${customPwm}% PWM request, ${zone.label}`}
            aria-describedby="fan-control-custom-pwm-readout"
            disabled={disabled}
          />
          <div id="fan-control-custom-pwm-readout" className="cp-fan-slider-readout">
            <span>
              PWM <span className="cp-fan-pwm-val">{customPwm}</span>
            </span>
            <span className="cp-fan-zone" data-zone={zone.zone}>{zone.label}</span>
          </div>
        </div>
      )}
    </div>
  );
}
