// DCENT_OS Setup Wizard — Review step.
//
// Structural recreation of the kit `ReviewStep` (ui_kits/wizard): the
// `.wiz-review` summary card — each row is a key / value / "edit →" button
// that jumps back to that step — plus the full-width "Apply configuration"
// CTA. The kit's review rows are clickable jump-backs (verbatim).
//
// Real wiring preserved: onEditStep jumps to the real step id; onApply runs
// the real applyConfig() chain (setupSafety/skipSafety, setupCircuit,
// setupMode, setupPool, updateDonationConfig, skipPassword, completeSetup,
// reboot) — UNCHANGED. The safety-acknowledgement checkbox stays a GENUINE
// gate (Apply disabled until checked). The honest "still idle until
// commissioned" framing is preserved (no fabricated mining-started claim).

import React, { useState } from 'react';
import type { OperatingMode } from '../../api/types';
import type { PoolConfig } from './PoolStep';

type StepId = 'welcome' | 'network' | 'power' | 'circuit' | 'pool' | 'donation' | 'name' | 'password' | 'mode' | 'calibration';
type SetupPath = 'quick' | 'guided';

interface ReviewStepProps {
  setupPath?: SetupPath;
  minerName: string;
  mode: OperatingMode;
  network?: string | null;
  powerSource: string | null;
  circuitVoltage: number | null;
  circuitAmperage: number | null;
  pool: PoolConfig;
  donationPercent?: number;
  donationEnabled?: boolean;
  password: string;
  safetyConfirmed: boolean;
  onSafetyConfirmedChange: (value: boolean) => void;
  onApply: () => Promise<void>;
  onEditStep?: (stepId: StepId) => void;
}

const MODE_LABEL: Record<OperatingMode, string> = {
  heater: 'Basic / Heating',
  standard: 'Standard / Mining',
  hacker: 'Advanced / Hacking',
};
const POWER_SOURCE_LABELS: Record<string, string> = {
  grid: 'Grid AC',
  direct_dc: 'Direct DC',
  solar_battery: 'Solar + Battery',
  hybrid: 'Grid + Solar Hybrid',
};
const NETWORK_LABELS: Record<string, string> = {
  eth: 'Ethernet',
  wifi: 'Wi-Fi (via Expansion Pack)',
  xpack: 'DCENT Expansion Pack',
};

export function ReviewStep({
  setupPath = 'guided',
  minerName,
  mode,
  network,
  powerSource,
  circuitVoltage,
  circuitAmperage,
  pool,
  donationPercent,
  donationEnabled,
  password,
  safetyConfirmed,
  onSafetyConfirmedChange,
  onApply,
  onEditStep,
}: ReviewStepProps) {
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const hasPool = pool.url.length > 0 && pool.worker.trim().length > 0;
  const hasPowerCommissioning = Boolean(circuitVoltage && circuitAmperage);
  const canStartMining = hasPool && hasPowerCommissioning;
  const isQuickStart = setupPath === 'quick';

  async function handleApply() {
    setApplying(true);
    setError(null);
    try {
      await onApply();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to apply settings');
      setApplying(false);
    }
  }

  const rows: { k: string; v: string; to: StepId }[] = [
    { k: 'Network', v: network ? (NETWORK_LABELS[network] ?? network) : 'Auto-detected', to: 'network' },
    { k: 'Password', v: password ? `••• set (${password.length} chars)` : 'not set', to: 'password' },
    { k: 'Mode', v: MODE_LABEL[mode], to: 'mode' },
    { k: 'Pool', v: pool.url ? `${pool.url}${pool.worker ? ` · ${pool.worker}` : ''}` : 'Not configured', to: 'pool' },
    {
      k: 'Circuit',
      v: circuitVoltage && circuitAmperage
        ? `${circuitVoltage} V × ${circuitAmperage} A`
        : 'Not declared',
      to: 'circuit',
    },
    {
      k: 'Power',
      v: powerSource ? (POWER_SOURCE_LABELS[powerSource] ?? powerSource) : 'Not declared',
      to: 'power',
    },
    {
      k: 'Donation',
      v: donationEnabled && (donationPercent ?? 0) > 0
        ? `${(donationPercent ?? 0).toFixed(1)}% voluntary`
        : 'none (0 %)',
      to: 'donation',
    },
    { k: 'Name', v: `${minerName || 'My Miner'}`, to: 'name' },
  ];

  return (
    <div className="wiz-step-body wizard-step-pane">
      <h2 className="wiz-h2">Review</h2>
      <p className="wiz-lede">
        Take a last look. Anything wrong? Tap a row to jump back. Apply writes the
        config and reboots the daemon. Mining only starts after setup explicitly
        enables it.
      </p>

      {!canStartMining && (
        <div className="wiz-info amber">
          <strong>Still missing mining commissioning.</strong> The miner will stay in
          dashboard-ready idle mode until you finish owner auth, pool setup, and the
          required power commissioning for your deployment. You can do this later.
        </div>
      )}

      <div className="wiz-review">
        {rows.map(r => (
          <button
            key={r.k}
            type="button"
            className="wiz-review-row"
            onClick={() => onEditStep?.(r.to)}
            aria-label={`Edit ${r.k}`}
          >
            <span className="wiz-review-k">{r.k}</span>
            <span className="wiz-review-v">{r.v}</span>
            <span className="wiz-review-edit">edit →</span>
          </button>
        ))}
      </div>

      {isQuickStart && (
        <div className="wiz-info">
          <strong>Deferred - finish later:</strong> power source, circuit check,
          calibration, miner name, and home comfort. Mode is saved as Standard.
          Donation stays at the miner default until you change it.
        </div>
      )}

      <div className="wiz-info">
        <strong>What happens next:</strong> settings are saved to the miner → the
        miner reboots (~60 s) →{' '}
        {canStartMining
          ? (mode === 'heater'
              ? 'mining starts with your pool config while keeping heater-mode UX'
              : 'mining starts automatically with your pool config')
          : 'the dashboard returns in safe idle mode — finish pool/power commissioning when you are ready'}{' '}
        → reconnect at the same address.
      </div>

      {error && (
        <div className="wiz-info danger" role="alert">
          {error}
        </div>
      )}

      <label className="wiz-review-ack">
        <input
          type="checkbox"
          checked={safetyConfirmed}
          onChange={e => onSafetyConfirmedChange(e.target.checked)}
        />
        <span>
          I confirm the miner has safe airflow, the power source and circuit details
          are correct for this install, and it is okay to save settings and reboot now.
        </span>
      </label>

      <button
        type="button"
        className="wiz-btn primary lg full"
        onClick={handleApply}
        disabled={applying || !safetyConfirmed}
      >
        {applying
          ? 'Applying… restarting daemon…'
          : !canStartMining
            ? 'Save idle setup & reboot'
            : mode === 'heater'
              ? 'Start mining in Heater mode'
              : mode === 'hacker'
                ? 'Apply & restart'
                : 'Start mining'}
      </button>

      {!applying && !safetyConfirmed && (
        <p className="wiz-fld-hint" style={{ textAlign: 'center' }}>
          Confirm the safety checklist above to enable Apply.
        </p>
      )}
    </div>
  );
}
