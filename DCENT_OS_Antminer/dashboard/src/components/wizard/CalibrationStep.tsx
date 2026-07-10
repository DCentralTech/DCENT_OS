// DCENT_OS Setup Wizard — Calibrate step.
//
// Structural recreation of the kit `CalibrationStep` (ui_kits/wizard): the
// `.wiz-cal` panel — labelled measured-input row, the meta line, the run/
// progress track, and the green "Calibrated" receipt.
//
// HONESTY (per brief): the kit calibrates "measured PSU output voltage".
// Production has NO Vout-calibration endpoint; the REAL calibration is the
// operator wall-wattmeter J/TH anchor (api.getPerfEfficiency +
// api.postPerfCalibrate). We keep the kit's panel STRUCTURE but wire it to
// the real wattmeter call — no fabricated voltage probe. The
// CalibrationStepValue / DEFAULT_CALIBRATION_STEP_VALUE exports are
// preserved exactly. This step is OPTIONAL/skippable on the rail.

import React, { useEffect, useState } from 'react';
import { api } from '../../api/client';
import type { PerfCalibrateResponse, PerfEfficiencyResponse } from '../../api/client';

export interface CalibrationStepValue {
  measuredWallWatts: number | null;
  jPerTh: number | null;
}

interface CalibrationStepProps {
  value: CalibrationStepValue;
  onChange: (value: CalibrationStepValue) => void;
  onSkip?: () => void;
}

export function CalibrationStep({ value, onChange, onSkip }: CalibrationStepProps) {
  const [draftWatts, setDraftWatts] = useState<string>(
    value.measuredWallWatts != null ? value.measuredWallWatts.toString() : '',
  );
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hint, setHint] = useState<PerfEfficiencyResponse | null>(null);

  useEffect(() => {
    let cancelled = false;
    api.getPerfEfficiency()
      .then(r => { if (!cancelled) setHint(r); })
      .catch(() => { if (!cancelled) setHint(null); });
    return () => { cancelled = true; };
  }, []);

  const handleSave = async () => {
    setError(null);
    const watts = Number(draftWatts);
    if (!Number.isFinite(watts) || watts < 50 || watts > 5000) {
      setError('Enter a wattmeter reading between 50 W and 5000 W.');
      return;
    }
    setSubmitting(true);
    try {
      const res: PerfCalibrateResponse = await api.postPerfCalibrate({ measured_wall_watts: watts });
      if (res.status !== 'ok') {
        setError(res.message ?? 'Calibration failed.');
        return;
      }
      onChange({ measuredWallWatts: watts, jPerTh: res.j_per_th ?? null });
      try {
        const updated = await api.getPerfEfficiency();
        setHint(updated);
      } catch { /* ignore */ }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSubmitting(false);
    }
  };

  const handleSkip = () => {
    onChange({ measuredWallWatts: null, jPerTh: null });
    onSkip?.();
  };

  const sourceLabel =
    hint?.source === 'operator' ? 'operator wattmeter'
    : hint?.source === 'pmbus' ? 'PSU PMBus telemetry'
    : 'modeled estimate (no wattmeter)';

  return (
    <div className="wiz-step-body wizard-step-pane">
      <h2 className="wiz-h2">Calibrate power</h2>
      <p className="wiz-lede">
        Optional. Measure your actual wall draw with an external meter (Kill-A-Watt,
        smart plug, breaker clamp) so the firmware&apos;s J/TH math is honest. Skip if
        you don&apos;t have a meter — we&apos;ll fall back to a modeled estimate (and
        say so).
      </p>

      <div className="wiz-cal">
        {hint?.j_per_th != null && (
          <div className="wiz-cal-meta">
            <span>
              Currently reporting{' '}
              <strong>{hint.j_per_th.toFixed(1)} J/TH</strong> from {sourceLabel}
            </span>
          </div>
        )}

        <div className="wiz-cal-row">
          <label htmlFor="wiz-cal-watts">Measured wall draw</label>
          <div className="wiz-cal-input">
            <input
              id="wiz-cal-watts"
              type="number"
              inputMode="numeric"
              min={50}
              max={5000}
              step={1}
              value={draftWatts}
              placeholder="e.g. 1310"
              aria-describedby={error ? 'wiz-cal-error' : undefined}
              onChange={e => setDraftWatts(e.target.value)}
            />
            <span>W</span>
          </div>
        </div>

        <div className="wiz-cal-meta">
          <span>Range: <strong>50–5000 W</strong></span>
          <span>Read it at a steady operating point, not during ramp-up.</span>
        </div>

        <button
          type="button"
          className="wiz-btn primary"
          onClick={handleSave}
          disabled={submitting || draftWatts.trim() === ''}
          aria-disabled={submitting || draftWatts.trim() === ''}
        >
          {submitting ? 'Calibrating…' : value.jPerTh != null ? 'Run again' : 'Save wattmeter reading'}
        </button>

        {submitting && (
          <div className="wiz-cal-track">
            <div style={{ width: '60%' }} />
          </div>
        )}

        {error && (
          <div id="wiz-cal-error" role="alert" className="wiz-err">
            {error}
          </div>
        )}

        {value.jPerTh != null && (
          <div className="wiz-cal-result" role="status" aria-live="polite">
            <strong>✓ Calibrated.</strong>
            <span>
              {value.measuredWallWatts} W → {value.jPerTh.toFixed(1)} J/TH
              (operator-confirmed). The autotuner&apos;s efficiency mode will use this
              as the source of truth. Saved.
            </span>
          </div>
        )}
      </div>

      <div className="wiz-info">
        <strong>Why this matters.</strong> The autotuner picks setpoints by joules per
        terahash. If the PSU&apos;s actual draw drifts from the model, every J/TH
        number is off by the same amount. Let the miner run a few minutes at your
        real operating point before reading the meter.
      </div>

      {onSkip && (
        <button type="button" className="wiz-btn lg full" onClick={handleSkip}>
          Skip — use modeled estimate
        </button>
      )}
    </div>
  );
}

export const DEFAULT_CALIBRATION_STEP_VALUE: CalibrationStepValue = {
  measuredWallWatts: null,
  jPerTh: null,
};
