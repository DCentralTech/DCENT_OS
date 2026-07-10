// DCENT_OS Setup Wizard — Power source step.
//
// Structural recreation of the kit `PowerSourceStep` (ui_kits/wizard):
// the power-source card grid (glyph + title + sub + detail), the active
// glow, and the inline circuit declaration that grid/hybrid installs need
// (production parity — the kit folds circuit into a separate step but
// production gates the grid/hybrid circuit here too, so it stays).
//
// Real wiring preserved: the production source IDs
// (grid | direct_dc | solar_battery | hybrid) are UNCHANGED — they drive
// api.setupCircuit({source}) and ReviewStep's POWER_SOURCE_LABELS. The
// exact props signature SetupWizard passes is preserved.

import React, { useMemo } from 'react';
import { computeMaxContinuousWatts, CIRCUIT_DEFAULT_DERATE } from './CircuitConfigStep';

const CIRCUIT_VOLTAGES = [120, 240] as const;
const CIRCUIT_AMPERAGES = [15, 20] as const;

interface PowerSource {
  id: string;
  glyph: string;
  l: string;
  sub: string;
  detail: string;
}

const POWER_SOURCES: PowerSource[] = [
  {
    id: 'grid',
    glyph: '~',
    l: 'Grid AC',
    sub: 'Standard wall outlet from the utility.',
    detail: 'The default. Reliable, always-on. Works with any Bitmain PSU (APW3–APW17). The autotuner can run any policy.',
  },
  {
    id: 'direct_dc',
    glyph: '◰',
    l: 'Direct DC',
    sub: 'Raw DC — battery, bench supply, generator.',
    detail: 'For dedicated DC-fed installs. Battery/DC protection is commissioned after first boot in Off-Grid.',
  },
  {
    id: 'solar_battery',
    glyph: '☀',
    l: 'Solar + Battery',
    sub: 'Solar panels with a battery bank.',
    detail: 'Tracks PV output / battery SOC and ramps hashrate to protect the bank. Provider policy is tuned later in Green Mining.',
  },
  {
    id: 'hybrid',
    glyph: '⊲',
    l: 'Grid + Solar Hybrid',
    sub: 'Mine 24/7 on grid, absorb solar surplus.',
    detail: 'AC-backed with live hybrid import minimization. Starts with a normal circuit declaration; solar behavior is tuned later.',
  },
];

interface PowerSourceStepProps {
  onSourceChange: (source: string) => void;
  currentSource: string | null;
  circuitVoltage: number | null;
  circuitAmperage: number | null;
  onCircuitVoltageChange: (voltage: number) => void;
  onCircuitAmperageChange: (amperage: number) => void;
}

export function PowerSourceStep({
  onSourceChange,
  currentSource,
  circuitVoltage,
  circuitAmperage,
  onCircuitVoltageChange,
  onCircuitAmperageChange,
}: PowerSourceStepProps) {
  const requiresCircuit = currentSource === 'grid' || currentSource === 'hybrid';

  const maxW = useMemo(() => {
    if (circuitVoltage === null || circuitAmperage === null) return null;
    return computeMaxContinuousWatts({
      voltage: circuitVoltage,
      amperage: circuitAmperage,
      derate: CIRCUIT_DEFAULT_DERATE,
    });
  }, [circuitVoltage, circuitAmperage]);

  return (
    <div className="wiz-step-body wizard-step-pane">
      <h2 className="wiz-h2">Power source</h2>
      <p className="wiz-lede">
        Where does this miner&apos;s electricity come from? AC circuits are declared
        here; DC and solar setups get smarter scheduling and finish commissioning
        after first boot.
      </p>

      <div className="wiz-power-grid" role="radiogroup" aria-label="Power source">
        {POWER_SOURCES.map(p => {
          const isActive = currentSource === p.id;
          return (
            <button
              key={p.id}
              type="button"
              role="radio"
              aria-checked={isActive}
              className={`wiz-power-card${isActive ? ' active' : ''}${isActive ? ' wizard-tile-selected-halo' : ''}`}
              onClick={() => onSourceChange(p.id)}
              title={p.detail}
            >
              <span className="wiz-power-icon" aria-hidden="true">{p.glyph}</span>
              <strong>{p.l}</strong>
              <span>{p.sub}</span>
              <small>{p.detail}</small>
            </button>
          );
        })}
      </div>

      {requiresCircuit && (
        <div className="wiz-donation" style={{ gap: 12 }}>
          <div>
            <div style={{ fontFamily: 'var(--wz-font-heading)', fontWeight: 700, fontSize: '1rem', color: 'var(--wz-fg-primary)', marginBottom: 4 }}>
              Circuit declaration
            </div>
            <p className="wiz-fld-hint" style={{ marginTop: 0 }}>
              Tell DCENT_OS what wall circuit this miner will share so residential
              limits can be applied more safely.
            </p>
          </div>
          <div className="wiz-circuit-grid">
            <div className="wiz-fld">
              <label htmlFor="wiz-power-v">Voltage</label>
              <select
                id="wiz-power-v"
                className="wiz-select"
                value={circuitVoltage ?? ''}
                onChange={e => onCircuitVoltageChange(Number(e.target.value))}
              >
                <option value="" disabled>Select voltage</option>
                {CIRCUIT_VOLTAGES.map(v => (
                  <option key={v} value={v}>{v} V</option>
                ))}
              </select>
            </div>
            <div className="wiz-fld">
              <label htmlFor="wiz-power-a">Breaker</label>
              <select
                id="wiz-power-a"
                className="wiz-select"
                value={circuitAmperage ?? ''}
                onChange={e => onCircuitAmperageChange(Number(e.target.value))}
              >
                <option value="" disabled>Select amperage</option>
                {CIRCUIT_AMPERAGES.map(a => (
                  <option key={a} value={a}>{a} A</option>
                ))}
              </select>
            </div>
          </div>
          {maxW !== null && (
            <div className="wiz-info amber">
              Safe continuous ceiling: <strong style={{ color: 'inherit' }}>{maxW} W</strong>{' '}
              after NEC 80% derate and 90% PSU efficiency. The autotuner refuses
              power-target raises above this.
            </div>
          )}
        </div>
      )}

      {currentSource && !requiresCircuit && (
        <div className="wiz-info">
          <strong>Off-grid friendly.</strong> DC-backed setups use a different
          commissioning path. After first boot, finish battery/DC commissioning in
          Off-Grid and configure solar provider policy in Green Mining. The autotuner
          ramps down before the supply collapses, not after.
        </div>
      )}
    </div>
  );
}
