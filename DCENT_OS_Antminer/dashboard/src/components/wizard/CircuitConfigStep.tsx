// DCENT_OS Setup Wizard — Circuit step.
//
// Structural recreation of the kit `CircuitStep` (ui_kits/wizard/Wizard.jsx):
// the 4 preset circuit cards (spec + continuous-watt cap + label + detail)
// and the warn/ok summary panel ("Continuous budget" vs "S19j Pro stock
// draw" + the safety verdict). A "Custom" tab is kept so non-NA / lab
// circuits stay declarable (production parity).
//
// Real math preserved: computeMaxContinuousWatts (V·A·PSU-eff·NEC-derate)
// and the CIRCUIT_DEFAULT_DERATE / CIRCUIT_PSU_EFFICIENCY exports that
// PowerSourceStep + SetupWizard import. The onSkip opt-out is the exact
// freedom-first parallel of PasswordStep's skip (drives api.skipSafety).

import React, { useMemo, useState } from 'react';

const DEFAULT_DERATE = 0.8;     // NEC 210.20(A) 80% continuous-load factor
const PSU_EFFICIENCY = 0.9;     // Wall-to-DC PSU efficiency

export interface CircuitConfig {
  voltage: number | null;
  amperage: number | null;
  derate: number;
}

export function computeMaxContinuousWatts(cfg: CircuitConfig): number | null {
  if (cfg.voltage === null || cfg.amperage === null) return null;
  if (!Number.isFinite(cfg.voltage) || !Number.isFinite(cfg.amperage)) return null;
  if (cfg.voltage <= 0 || cfg.amperage <= 0) return null;
  const derate = Math.max(0.5, Math.min(1.0, cfg.derate || DEFAULT_DERATE));
  return Math.round(cfg.voltage * cfg.amperage * PSU_EFFICIENCY * derate);
}

interface CircuitConfigStepProps {
  voltage: number | null;
  amperage: number | null;
  derate?: number;
  onVoltageChange: (v: number) => void;
  onAmperageChange: (a: number) => void;
  onDerateChange?: (d: number) => void;
  /** Freedom-first: present when skipping the circuit/safety check is
   *  allowed — the exact parallel of PasswordStep's onSkip. */
  onSkip?: () => void;
}

interface Preset {
  v: number;
  a: number;
  l: string;
  sub: string;
}

const PRESETS: Preset[] = [
  { v: 120, a: 15, l: 'Residential 120 V / 15 A', sub: 'Most US bedroom / office circuits' },
  { v: 120, a: 20, l: 'Residential 120 V / 20 A', sub: 'Kitchen / garage circuits' },
  { v: 240, a: 15, l: '240 V / 15 A',             sub: 'Dedicated branch · dryer-style' },
  { v: 240, a: 30, l: '240 V / 30 A',             sub: 'Stove-class. Plenty of headroom' },
];

type Mode = 'preset' | 'custom';

export function CircuitConfigStep({
  voltage,
  amperage,
  derate = DEFAULT_DERATE,
  onVoltageChange,
  onAmperageChange,
  onDerateChange,
  onSkip,
}: CircuitConfigStepProps) {
  const [mode, setMode] = useState<Mode>('preset');
  const [customVoltage, setCustomVoltage] = useState<string>(voltage?.toString() ?? '');
  const [customAmperage, setCustomAmperage] = useState<string>(amperage?.toString() ?? '');

  const cap = useMemo(
    () => computeMaxContinuousWatts({ voltage, amperage, derate }),
    [voltage, amperage, derate],
  );
  // S19j Pro stock wall draw, mirrors the kit's reference figure.
  const S19J_STOCK_W = 3200;
  const hasValues = voltage !== null && amperage !== null;
  const over = hasValues && cap !== null && cap < S19J_STOCK_W;

  function applyCustom() {
    const v = Number(customVoltage);
    const a = Number(customAmperage);
    if (Number.isFinite(v) && v > 0) onVoltageChange(v);
    if (Number.isFinite(a) && a > 0) onAmperageChange(a);
  }

  return (
    <div className="wiz-step-body wizard-step-pane">
      <h2 className="wiz-h2">Declare your circuit</h2>
      <p className="wiz-lede">
        We use this to surface a safety warning before your breaker trips. NEC says
        continuous loads must stay under 80 % of the circuit rating. The autotuner
        refuses to push power above this ceiling.
      </p>

      <div className="wiz-tabs" role="tablist" aria-label="Circuit declaration mode">
        {(['preset', 'custom'] as Mode[]).map(m => (
          <button
            key={m}
            type="button"
            role="tab"
            aria-selected={mode === m}
            className={`wiz-tab${mode === m ? ' active' : ''}`}
            onClick={() => setMode(m)}
          >
            {m === 'preset' ? 'Standard circuit' : 'Custom'}
          </button>
        ))}
      </div>

      {mode === 'preset' && (
        <div className="wiz-circuit-grid" role="radiogroup" aria-label="Circuit preset">
          {PRESETS.map(p => {
            const isActive = voltage === p.v && amperage === p.a;
            const pCap = computeMaxContinuousWatts({ voltage: p.v, amperage: p.a, derate });
            return (
              <button
                key={`${p.v}.${p.a}`}
                type="button"
                role="radio"
                aria-checked={isActive}
                className={`wiz-circuit-card${isActive ? ' active' : ''}`}
                onClick={() => { onVoltageChange(p.v); onAmperageChange(p.a); }}
              >
                <div className="wiz-circuit-spec">
                  {p.v} V <span>×</span> {p.a} A
                </div>
                <div className="wiz-circuit-cap">
                  {pCap} W <small>continuous</small>
                </div>
                <div className="wiz-circuit-sub">{p.l}</div>
                <div className="wiz-circuit-detail">{p.sub}</div>
              </button>
            );
          })}
        </div>
      )}

      {mode === 'custom' && (
        <div className="wiz-circuit-grid">
          <div className="wiz-fld">
            <label htmlFor="wiz-circuit-v">Voltage (V)</label>
            <input
              id="wiz-circuit-v"
              className="wiz-input"
              type="number"
              min={48}
              max={480}
              step={1}
              inputMode="numeric"
              value={customVoltage}
              onChange={e => setCustomVoltage(e.target.value)}
              onBlur={applyCustom}
              placeholder="e.g. 208"
            />
          </div>
          <div className="wiz-fld">
            <label htmlFor="wiz-circuit-a">Breaker (A)</label>
            <input
              id="wiz-circuit-a"
              className="wiz-input"
              type="number"
              min={5}
              max={100}
              step={1}
              inputMode="numeric"
              value={customAmperage}
              onChange={e => setCustomAmperage(e.target.value)}
              onBlur={applyCustom}
              placeholder="e.g. 30"
            />
          </div>
        </div>
      )}

      {onDerateChange && (
        <div className="wiz-fld">
          <label htmlFor="wiz-circuit-derate" style={{ display: 'flex', justifyContent: 'space-between' }}>
            <span>NEC continuous-load derate</span>
            <span style={{ color: 'var(--wz-accent)' }}>{Math.round(derate * 100)}%</span>
          </label>
          <input
            id="wiz-circuit-derate"
            className="wiz-input-range"
            type="range"
            min={0.5}
            max={1.0}
            step={0.05}
            value={derate}
            onChange={e => onDerateChange(Number(e.target.value))}
            aria-label="NEC continuous load derate factor"
            aria-valuetext={`${Math.round(derate * 100)}%`}
          />
          <small className="wiz-fld-hint">
            Mining is a 24/7 continuous load. NEC 210.20(A) requires 80% derate. Lower
            this only if your circuit is rated for 100% continuous duty.
          </small>
        </div>
      )}

      <div className={`wiz-circuit-summary ${over ? 'warn' : hasValues ? 'ok' : ''}`}>
        <div>
          <div className="wiz-circuit-summary-label">Continuous budget</div>
          <div className="wiz-circuit-summary-val">{cap === null ? '—' : `${cap} W`}</div>
        </div>
        <div>
          <div className="wiz-circuit-summary-label">S19j Pro stock draw</div>
          <div className="wiz-circuit-summary-val">~3,200 W</div>
        </div>
        <div className="wiz-circuit-summary-status">
          {!hasValues ? (
            <>Pick voltage and breaker amps above to compute the safe ceiling.</>
          ) : over ? (
            <>
              <span className="wiz-circuit-summary-icon warn" aria-hidden="true">!</span>
              This circuit can&apos;t safely run an S19j Pro at stock. The autotuner
              will cap power to {cap} W.
            </>
          ) : (
            <>
              <span className="wiz-circuit-summary-icon ok" aria-hidden="true">✓</span>
              Plenty of headroom for an S19j Pro.
            </>
          )}
        </div>
      </div>

      {onSkip && voltage === null && amperage === null && (
        <>
          <div className="wiz-info amber">
            <strong>Recommended (not required) — verify your circuit.</strong>{' '}
            Declaring your circuit keeps the autotuner from pushing power past your
            breaker (the #1 home-mining gotcha: tripped breakers, melted connectors).
            You can run without it and add it later in Settings — your call.
          </div>
          <button type="button" className="wiz-btn lg full" onClick={onSkip}>
            Continue without the circuit check
          </button>
        </>
      )}
    </div>
  );
}

export const CIRCUIT_DEFAULT_DERATE = DEFAULT_DERATE;
export const CIRCUIT_PSU_EFFICIENCY = PSU_EFFICIENCY;
