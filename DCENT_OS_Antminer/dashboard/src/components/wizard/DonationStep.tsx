// DCENT_OS Setup Wizard — Donation step.
//
// Structural recreation of the kit `DonationStep` (ui_kits/wizard): the
// enable toggle row, the 0–5% slider with the big live percentage readout,
// the preset chips, and the time-split "how it works" impact panel.
//
// Truth-contract preserved verbatim (load-bearing — do NOT soften):
//   • "no mandatory dev fee" framing, configurable 0–5%, fully disableable;
//   • the always-visible "DONATING" pill is described, not implied;
//   • donation destination disclosed (pool.d-central.tech / DungeonMaster);
//   • the impact panel shows the REAL hour-cycle time split (donateS/userS
//     from cycle 3600 s) — NOT fabricated sats/USD earnings. We never invent
//     an earnings number; the panel is the honest "X s per hour" math.
//
// DonationStepValue / onChange contract preserved exactly so SetupWizard's
// api.updateDonationConfig call is unaffected.

import React from 'react';
import type { OperatingMode } from '../../api/types';

export interface DonationStepValue {
  enabled: boolean;
  percent: number;
}

interface DonationStepProps {
  value: DonationStepValue;
  mode: OperatingMode | null;
  onChange: (next: DonationStepValue) => void;
}

const PRESETS = [0, 0.5, 1, 2, 3, 5];
const DEFAULT_PERCENT = 2;
const CYCLE_S = 3600;

function clampPercent(v: number): number {
  return Math.max(0, Math.min(5, v));
}
function fmt(v: number): string {
  return v.toFixed(v % 1 === 0 ? 0 : 1);
}

export function DonationStep({ value, mode, onChange }: DonationStepProps) {
  const enabled = value.enabled;
  const percent = clampPercent(value.percent ?? DEFAULT_PERCENT);
  const donateS = Math.round((percent / 100) * CYCLE_S);
  const userS = CYCLE_S - donateS;

  const toggle = () =>
    onChange({
      enabled: !enabled,
      percent: !enabled && percent <= 0 ? DEFAULT_PERCENT : percent,
    });
  const setPercent = (p: number) => onChange({ enabled, percent: clampPercent(p) });

  return (
    <div className="wiz-step-body wizard-step-pane">
      <h2 className="wiz-h2">Optional donation</h2>
      <p className="wiz-fld-hint">
        optional, not mandatory: unlike a fee you cannot disable, you choose the amount and can disable it.
      </p>
      <p className="wiz-lede">
        DCENT_OS has <strong>no mandatory dev fee</strong>. The donation keeps the
        firmware funded by briefly switching to DCENT_Pool — D-Central&apos;s
        Solo/Guild pool — then returning to your pool. Unlike a fee you cannot
        disable, you choose the amount, increase it up to 5%, set it to 0%, or
        disable it entirely.
      </p>

      <div className="wiz-info amber">
        <strong>Please leave at least 1% enabled.</strong> This is the project&apos;s
        revenue model. If the firmware helps your miner, the donation helps keep
        open-source mining firmware maintained.
      </div>

      <div className={`wiz-donation-toggle-row${enabled ? ' on' : ''}`}>
        <div>
          <div
            style={{
              fontFamily: 'var(--wz-font-heading)',
              fontWeight: 700,
              fontSize: '1rem',
              color: enabled ? 'var(--wz-accent)' : 'var(--wz-fg-primary)',
            }}
          >
            {enabled ? `Donation enabled at ${fmt(percent)}%` : 'Donation disabled'}
          </div>
          <div style={{ color: 'var(--wz-fg-secondary)', fontSize: '.78rem', marginTop: 2 }}>
            {enabled
              ? `${donateS}s per hour on the donation pool / ${userS}s on your pool`
              : 'Your miner stays on your pool for the full cycle.'}
          </div>
        </div>
        <button
          type="button"
          role="switch"
          aria-checked={enabled}
          aria-label={enabled ? 'Disable donation' : 'Enable donation'}
          onClick={toggle}
          className={`ds-toggle${enabled ? ' on' : ''}`}
        >
          <span className="ds-toggle-knob" />
        </button>
      </div>

      <div className="wiz-donation">
        <div className="wiz-donation-slider-row">
          <input
            id="wiz-donation-percent"
            className="wiz-input-range"
            type="range"
            min={0}
            max={5}
            step={0.5}
            value={percent}
            disabled={!enabled}
            onChange={e => setPercent(Number(e.target.value))}
            aria-label="Donation percent"
            aria-valuetext={`${fmt(percent)} percent`}
          />
          <div className="wiz-donation-val">
            <strong>{fmt(percent)}</strong>
            <small>%</small>
          </div>
        </div>

        <div className="wiz-donation-presets">
          {PRESETS.map(p => (
            <button
              key={p}
              type="button"
              className={`wiz-donation-preset${Math.abs(percent - p) < 0.05 ? ' active' : ''}`}
              onClick={() => enabled && setPercent(p)}
              disabled={!enabled}
              aria-label={`Set donation to ${p} percent`}
            >
              {p}%{p === 2 ? ' rec' : ''}
            </button>
          ))}
        </div>

        <div className="wiz-donation-impact">
          <div>
            <span>On your pool</span>
            <strong>{userS}s / hour</strong>
          </div>
          <div>
            <span>On donation pool</span>
            <strong style={{ color: 'var(--wz-accent)' }}>{donateS}s / hour</strong>
          </div>
          <div>
            <span>That&apos;s about</span>
            <strong>{((donateS / CYCLE_S) * 100).toFixed(1)}% of hashtime</strong>
          </div>
        </div>
      </div>

      <div className="wiz-info">
        <strong>Always visible.</strong> Whenever donation is active, a
        &quot;DONATING&quot; pill shows on the dashboard top bar so you can see
        exactly what&apos;s happening. The autotuner typically recovers the cost in
        efficiency gains.
        {mode === 'heater' && <> At 2%, that&apos;s 72 seconds inside a 60-minute cycle.</>}
      </div>

      <p className="wiz-fld-hint">
        Donation destination: DCENT_Pool (pool.d-central.tech), worker
        DungeonMaster — D-Central&apos;s Solo/Guild pool. You can change or
        disable the donation anytime under Settings.
      </p>
    </div>
  );
}

export default DonationStep;
