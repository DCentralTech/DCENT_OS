// DCENT_OS Setup Wizard — Home comfort & costs step (P2-4, §4.E).
//
// Heater/home-mode-only step (auto-skipped in Standard/Hacker by the wizard's
// visibility gate). Captures the two home-operator economics/comfort inputs the
// setup flow previously never asked for:
//
//   (a) Electricity rate + currency — persisted to the DAEMON config
//       ([home].electricity_rate / [home].currency) via api.setupEconomics, so
//       cost/earnings math has a single source of truth and stops reading as an
//       "uncalibrated estimate". (The old client default was a localStorage
//       0.10 guess that disagreed with the daemon's 0.12.)
//
//   (b) Quiet hours — the [home.night_mode] defaults (22:00–07:00, fan PWM ≤ 30,
//       −40% power) exist in config but setup never prompted. SetupWizard wires
//       these to api.setupQuietHours (the setup-namespaced [home.night_mode]
//       writer, which clamps the fan PWM to the home safety ceiling). Cut hash
//       before raising noise.
//
// HONEST: no value is fabricated. Leaving the rate blank keeps the daemon
// default (and the dashboard keeps labelling earnings "uncalibrated"); leaving
// quiet hours off persists nothing. Both are skippable.

import React from 'react';
import { InfoDot } from '../common/Tooltip';

export interface HomeComfortStepValue {
  /** Electricity rate text draft ($/kWh). Empty = keep daemon default (uncalibrated). */
  electricityRate: string;
  /** Display currency code (e.g. "USD"). */
  currency: string;
  /** Whether to enable quiet hours. */
  quietHoursEnabled: boolean;
  /** Quiet-hours start hour (0–23). */
  quietStartHour: number;
  /** Quiet-hours end hour (0–23). */
  quietEndHour: number;
  /** Power reduction during quiet hours (0–100 %). */
  quietPowerReductionPct: number;
}

export const DEFAULT_HOME_COMFORT_STEP_VALUE: HomeComfortStepValue = {
  electricityRate: '',
  currency: 'USD',
  quietHoursEnabled: false,
  quietStartHour: 22,
  quietEndHour: 7,
  quietPowerReductionPct: 40,
};

// The fan PWM ceiling for home/quiet operation is fixed at 30 (load-bearing
// safety cap). Shown read-only here; the daemon clamps regardless.
const HOME_FAN_PWM_CAP = 30;

const CURRENCIES = ['USD', 'CAD', 'EUR', 'GBP', 'AUD', 'MXN'] as const;

interface HomeComfortStepProps {
  value: HomeComfortStepValue;
  /** Daemon-reported default rate (so the placeholder shows the real default). */
  defaultRate?: number;
  /** Whether the operator has already confirmed a rate on the daemon. */
  alreadyCalibrated?: boolean;
  onChange: (value: HomeComfortStepValue) => void;
}

function clampHour(n: number): number {
  if (!Number.isFinite(n)) return 0;
  return Math.max(0, Math.min(23, Math.round(n)));
}

export function HomeComfortStep({ value, defaultRate = 0.12, alreadyCalibrated, onChange }: HomeComfortStepProps) {
  const set = (patch: Partial<HomeComfortStepValue>) => onChange({ ...value, ...patch });

  const rateNum = Number(value.electricityRate);
  const rateValid = value.electricityRate.trim() === '' || (Number.isFinite(rateNum) && rateNum >= 0 && rateNum <= 10);

  return (
    <div className="wiz-step-body wizard-step-pane">
      <h2 className="wiz-h2">Home comfort & costs</h2>
      <p className="wiz-lede">
        Optional. Tell DCENT_OS your power price so cost and earnings figures are
        honest, and set quiet hours so the heater stays calm overnight. Both are
        skippable — leave the rate blank to keep the default and we'll keep
        labelling earnings an uncalibrated estimate.
      </p>

      {/* ─── Electricity rate ─────────────────────────────── */}
      <div className="wiz-cal">
        <div className="wiz-cal-row">
          <label htmlFor="wiz-home-rate">
            Electricity rate <InfoDot term="daily_cost" size={12} />
          </label>
          <div className="wiz-cal-input">
            <input
              id="wiz-home-rate"
              type="number"
              inputMode="decimal"
              min={0}
              max={10}
              step={0.01}
              value={value.electricityRate}
              placeholder={defaultRate.toFixed(2)}
              aria-describedby={!rateValid ? 'wiz-home-rate-err' : undefined}
              onChange={e => set({ electricityRate: e.target.value })}
            />
            <span>/ kWh</span>
          </div>
        </div>

        <div className="wiz-cal-row">
          <label htmlFor="wiz-home-currency">Currency</label>
          <div className="wiz-cal-input">
            <select
              id="wiz-home-currency"
              value={value.currency}
              onChange={e => set({ currency: e.target.value })}
            >
              {CURRENCIES.map(c => (
                <option key={c} value={c}>{c}</option>
              ))}
            </select>
          </div>
        </div>

        <div className="wiz-cal-meta">
          <span>Range: <strong>0–10 / kWh</strong></span>
          {alreadyCalibrated
            ? <span>A rate is already confirmed on this miner — re-enter to change it.</span>
            : <span>Until you confirm a rate, earnings show as an uncalibrated estimate.</span>}
        </div>

        {!rateValid && (
          <div id="wiz-home-rate-err" role="alert" className="wiz-err">
            Enter a rate between 0 and 10 per kWh, or leave it blank for the default.
          </div>
        )}
      </div>

      {/* ─── Quiet hours ──────────────────────────────────── */}
      <div className="wiz-info">
        <label className="wiz-toggle-row" style={{ display: 'flex', alignItems: 'center', gap: 8, cursor: 'pointer' }}>
          <input
            type="checkbox"
            checked={value.quietHoursEnabled}
            onChange={e => set({ quietHoursEnabled: e.target.checked })}
          />
          <strong>Enable quiet hours</strong>
        </label>
        <p style={{ margin: '6px 0 0' }}>
          During quiet hours the heater cuts power (cut hash before raising noise)
          and holds fans at PWM ≤ {HOME_FAN_PWM_CAP}. Defaults: 22:00–07:00, −40 % power.
          {' '}<InfoDot term="night_mode_behaviour" size={12} />
        </p>

        {value.quietHoursEnabled && (
          <div className="wiz-cal" style={{ marginTop: 10 }}>
            <div className="wiz-cal-row">
              <label htmlFor="wiz-quiet-start">Start hour</label>
              <div className="wiz-cal-input">
                <input
                  id="wiz-quiet-start"
                  type="number"
                  inputMode="numeric"
                  min={0}
                  max={23}
                  step={1}
                  value={value.quietStartHour}
                  onChange={e => set({ quietStartHour: clampHour(Number(e.target.value)) })}
                />
                <span>:00</span>
              </div>
            </div>
            <div className="wiz-cal-row">
              <label htmlFor="wiz-quiet-end">End hour</label>
              <div className="wiz-cal-input">
                <input
                  id="wiz-quiet-end"
                  type="number"
                  inputMode="numeric"
                  min={0}
                  max={23}
                  step={1}
                  value={value.quietEndHour}
                  onChange={e => set({ quietEndHour: clampHour(Number(e.target.value)) })}
                />
                <span>:00</span>
              </div>
            </div>
            <div className="wiz-cal-row">
              <label htmlFor="wiz-quiet-reduction">
                Power reduction <InfoDot term="power_budget" size={12} />
              </label>
              <div className="wiz-cal-input">
                <input
                  id="wiz-quiet-reduction"
                  type="number"
                  inputMode="numeric"
                  min={0}
                  max={100}
                  step={5}
                  value={value.quietPowerReductionPct}
                  onChange={e => set({
                    quietPowerReductionPct: Math.max(0, Math.min(100, Math.round(Number(e.target.value)) || 0)),
                  })}
                />
                <span>%</span>
              </div>
            </div>
            <div className="wiz-cal-meta">
              <span>Fan PWM capped at <strong>{HOME_FAN_PWM_CAP}</strong> overnight (safety ceiling).</span>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
