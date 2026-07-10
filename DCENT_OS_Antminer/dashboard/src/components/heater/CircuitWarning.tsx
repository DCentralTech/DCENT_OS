import React from 'react';

// W8.2 — non-dismissable circuit-budget warning banner.
//
// Shows in Standard mode whenever live wall-power draw exceeds the safe
// ceiling for the operator's declared circuit. The signal class:
//
// - 120 V / 15 A is the residential default. NEC continuous-load derate
//   (80%) drops the safe budget to ~1296 W. An S19 Pro or S21 at stock
//   power will trip the breaker. We surface this BEFORE the breaker does.
//
// - This banner is intentionally non-dismissable — the operator must
//   either lower the power target, switch modes, or change the circuit
//   declaration. Hiding it would defeat the whole point.
//
// - We deliberately accept this banner being prominent. A nuisance
//   warning is dramatically cheaper than an RMA, melted connector, or
//   house fire.

interface CircuitWarningProps {
  /// Live wall power telemetry (watts).
  currentWatts: number | null;
  /// Declared circuit voltage.
  voltageV: number | null;
  /// Declared breaker amperage.
  amperageA: number | null;
  /// Computed max continuous watts for this circuit.
  circuitCapacityW: number | null;
  /// Optional click handler — typically routes to power settings.
  onOpenPowerSettings?: () => void;
}

const RESIDENTIAL_120V_15A_THRESHOLD_W = 1440;

export function CircuitWarning({
  currentWatts,
  voltageV,
  amperageA,
  circuitCapacityW,
  onOpenPowerSettings,
}: CircuitWarningProps) {
  if (currentWatts === null || !Number.isFinite(currentWatts)) return null;

  const isResidential15A = voltageV === 120 && amperageA === 15;
  const overCap = circuitCapacityW !== null && currentWatts > circuitCapacityW;

  // The hard-coded 1440 W threshold is the spec from W8.2: any 120 V/15 A
  // circuit drawing >1440 W is past the NEC continuous-load ceiling
  // (1800 × 0.8 = 1440), even before applying PSU efficiency.
  const overResidential = isResidential15A && currentWatts > RESIDENTIAL_120V_15A_THRESHOLD_W;

  if (!overCap && !overResidential) return null;

  const cap = circuitCapacityW ?? RESIDENTIAL_120V_15A_THRESHOLD_W;
  const overBy = Math.max(0, currentWatts - cap);

  return (
    <div
      role="alert"
      aria-live="assertive"
      className="circuit-warning dcm-card-enter"
    >
      <div className="circuit-warning__icon" aria-hidden="true">
        {'⚠'}
      </div>

      <div className="circuit-warning__content">
        <div className="circuit-warning__title">
          Circuit budget exceeded
        </div>
        <div className="circuit-warning__body">
          {isResidential15A ? (
            <>
              This miner is drawing <strong>{Math.round(currentWatts)} W</strong>
              {' '}on a declared <strong>120 V / 15 A</strong> circuit.
              The NEC continuous-load ceiling is <strong>{cap} W</strong>
              {overBy > 0 ? <> ({Math.round(overBy)} W over)</> : null}.
              Lower the power target or move this unit to a 240 V circuit
              before the breaker trips.
            </>
          ) : (
            <>
              Wall draw <strong>{Math.round(currentWatts)} W</strong> exceeds
              the declared circuit budget of <strong>{cap} W</strong>
              {overBy > 0 ? <> by {Math.round(overBy)} W</> : null}.
              The autotuner will throttle, but please lower the power target
              or update your circuit declaration.
            </>
          )}
        </div>
        {onOpenPowerSettings && (
          <button
            type="button"
            className="circuit-warning__action"
            onClick={onOpenPowerSettings}
          >
            Open power settings
          </button>
        )}
      </div>
    </div>
  );
}

export const CIRCUIT_WARNING_THRESHOLD_120V_15A_W = RESIDENTIAL_120V_15A_THRESHOLD_W;
