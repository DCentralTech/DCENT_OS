// Circuit Capacity Calculator — NEC-compliant electrical safety check
// Feature no competitor has: built-in electrical safety validation

import React, { useState, useMemo } from 'react';
import type { CircuitVoltage, BreakerAmps, CircuitConfig, CircuitResult } from '../../api/feature-types';
import { useTranslation } from '../../i18n/i18n';
import { useMinerStore } from '../../store/miner';
import { TaskHandoffBanner } from '../common/TaskHandoffBanner';
import { InfoDot } from '../common/Tooltip';
import { getLiveWallWatts } from '../../utils/power';

const PLANNING_LOAD_WATTS = 1100;

function calculateCircuit(config: CircuitConfig, currentWatts: number): CircuitResult {
  const maxAmps = config.breakerAmps * (config.safetyFactorPct / 100);
  const maxWatts = maxAmps * config.voltage;
  const currentAmps = currentWatts / config.voltage;
  const usagePct = maxWatts > 0 ? (currentWatts / maxWatts) * 100 : 0;

  return {
    maxContinuousWatts: maxWatts,
    maxContinuousAmps: maxAmps,
    currentUsageWatts: currentWatts,
    usagePct,
    safe: currentWatts <= maxWatts,
  };
}

export function CircuitCalculator() {
  const { t } = useTranslation();
  const addAlert = useMinerStore(s => s.addAlert);
  const updateSettings = useMinerStore(s => s.updateSettings);
  const liveWatts = useMinerStore(s => getLiveWallWatts(s.stats?.power));
  const wattsIsLive = liveWatts > 0;
  const watts = wattsIsLive ? liveWatts : PLANNING_LOAD_WATTS;

  const [config, setConfig] = useState<CircuitConfig>({
    voltage: 240,
    breakerAmps: 20,
    safetyFactorPct: 80,
  });

  const result = useMemo(
    () => calculateCircuit(config, watts),
    [config, watts]
  );

  const update = (partial: Partial<CircuitConfig>) => {
    setConfig(prev => ({ ...prev, ...partial }));
  };

  // SAFETY-RELEVANT TRUTHFULNESS: this is a reference calculation, NOT a miner
  // power cap. dcentrald has no `updateConfig` key today that enforces a wall-watt
  // ceiling from this surface, so saying "power budget set" would be a dangerous
  // illusion — an operator could believe the miner is throttled to their breaker
  // when it is not. We record the computed safe ceiling as a LOCAL dashboard
  // reference (the same field SettingsView shows) and say exactly that.
  const handleSaveReference = () => {
    updateSettings({ powerBudgetWatts: Math.floor(result.maxContinuousWatts) });
    addAlert(
      'info',
      `Saved ${Math.floor(result.maxContinuousWatts)}W as a circuit reference (dashboard only). `
        + `This is NOT applied to the miner — it does not cap the hardware. `
        + `To actually limit draw, set a power/efficiency target in tuning.`,
    );
  };

  const usageColor = result.usagePct > 100 ? 'var(--feat-red)'
    : result.usagePct > 80 ? 'var(--feat-yellow)'
    : 'var(--feat-green)';

  const voltageId = 'circuit-voltage';
  const breakerId = 'circuit-breaker';
  const safetyFactorId = 'circuit-safety-factor';
  const safetyFactorHintId = 'circuit-safety-factor-hint';

  return (
    <div className="feat-page">
      <TaskHandoffBanner
        expectedMode="standard"
        title="Power safety task opened from Heater mode"
        copy="Validate the safe circuit budget here, then return to Heat view once the install ceiling is confirmed."
      />

      <div className="feat-header">
        <h2 className="feat-title feat-title-blue">{t('circuit.title')}</h2>
        <p className="feat-subtitle">{t('circuit.subtitle')}</p>
      </div>

      {/* Inputs */}
      <div className="feat-card">
        <div className="feat-form-grid">
          <div className="feat-input-group">
            <label className="feat-label" htmlFor={voltageId}>{t('circuit.voltage')}</label>
            <select
              id={voltageId}
              value={config.voltage}
              onChange={e => update({ voltage: Number(e.target.value) as CircuitVoltage })}
              className="feat-input"
            >
              <option value={120}>120V (North America standard)</option>
              <option value={240}>240V (Dryer/Range outlet)</option>
            </select>
          </div>

          <div className="feat-input-group">
            <label className="feat-label" htmlFor={breakerId}>{t('circuit.breaker')}</label>
            <select
              id={breakerId}
              value={config.breakerAmps}
              onChange={e => update({ breakerAmps: Number(e.target.value) as BreakerAmps })}
              className="feat-input"
            >
              <option value={15}>15A</option>
              <option value={20}>20A</option>
              <option value={30}>30A</option>
              <option value={40}>40A</option>
              <option value={50}>50A</option>
            </select>
          </div>

          <div className="feat-input-group">
            <label className="feat-label" htmlFor={safetyFactorId}>
              {t('circuit.safetyFactor')}
              <InfoDot
                placement="top"
                label="Why the 80% safety factor"
                content={
                  <>
                    A miner is a continuous load. The US electrical code (NEC
                    210.20) limits a continuous load to 80% of the breaker rating
                    — a 20 A breaker is only safe to ~16 A continuously. Staying
                    under this is what keeps the breaker from tripping or the
                    wiring from overheating. Lower it for older wiring.
                  </>
                }
              />
            </label>
            <div className="feat-safety-display">
              <input
                id={safetyFactorId}
                type="range"
                min="50"
                max="100"
                value={config.safetyFactorPct}
                onChange={e => update({ safetyFactorPct: Number(e.target.value) })}
                className="feat-range"
                aria-describedby={safetyFactorHintId}
              />
              <span className="feat-range-value" aria-live="polite">{config.safetyFactorPct}%</span>
            </div>
            <div className="feat-hint" id={safetyFactorHintId}>NEC Article 210.20 requires 80% for continuous loads</div>
          </div>
        </div>
      </div>

      {/* Results */}
      <div className="feat-card">
        <h3 className="feat-card-title">Results</h3>

        {/* Visual gauge */}
        <div className="feat-circuit-gauge">
          <div className="feat-circuit-bar-bg">
            <div
              className="feat-circuit-bar-fill"
              style={{
                width: `${Math.min(100, result.usagePct)}%`,
                background: usageColor,
              }}
            />
            {result.usagePct > 100 && (
              <div
                className="feat-circuit-bar-over"
                style={{ width: `${Math.min(100, result.usagePct - 100)}%` }}
              />
            )}
          </div>
          <div className="feat-circuit-gauge-labels">
            <span>0W</span>
            <span>{Math.floor(result.maxContinuousWatts)}W max</span>
          </div>
        </div>

        {/* Results grid */}
        <div className="feat-result-grid">
          <div className="feat-result-card feat-result-blue">
            <div className="feat-result-label">{t('circuit.safeLoad')}</div>
            <div className="feat-result-value">
              {Math.floor(result.maxContinuousWatts)} W
            </div>
            <div className="feat-result-sub">
              {result.maxContinuousAmps.toFixed(1)} A @ {config.voltage}V
            </div>
          </div>
          <div className="feat-result-card" style={{
            background: result.safe ? 'var(--feat-green-dim)' : 'var(--feat-red-dim)',
          }}>
            <div className="feat-result-label">
              {wattsIsLive ? t('circuit.currentUsage') : 'Planning load'}
            </div>
            <div className="feat-result-value" style={{ color: usageColor }}>
              {result.currentUsageWatts.toLocaleString()} W{!wattsIsLive ? ' assumed' : ''}
            </div>
            <div className="feat-result-sub">
              {wattsIsLive
                ? `${result.usagePct.toFixed(0)}% of safe capacity`
                : 'Sizing assumption only'}
            </div>
          </div>
          <div className="feat-result-card">
            <div className="feat-result-label">{t('circuit.remaining')}</div>
            <div className="feat-result-value" style={{
              color: result.safe ? 'var(--feat-green)' : 'var(--feat-red)',
            }}>
              {Math.max(0, Math.floor(result.maxContinuousWatts - result.currentUsageWatts))} W
            </div>
          </div>
        </div>

        {!wattsIsLive && (
          <p className="feat-hint">
            Planning load uses 1,100 W because live wall-power telemetry is unavailable;
            it is not a measured current draw.
          </p>
        )}

        {/* Safety indicator */}
        <div className={`feat-safety-banner ${result.safe ? 'feat-safe' : 'feat-unsafe'}`}>
          {result.safe ? t('circuit.safe') : t('circuit.overloaded')}
        </div>

        {/* Save-as-reference button. Honest label: this is a circuit-capacity
            calculation, not a hardware power cap (dcentrald has no live wall-watt
            ceiling wired to this surface). It records the safe ceiling as a
            dashboard reference only. */}
        <div className="feat-actions" style={{ marginTop: 16, flexDirection: 'column', alignItems: 'stretch', gap: 8 }}>
          <button type="button" className="feat-btn feat-btn-primary" onClick={handleSaveReference}>
            {t('circuit.applyBudget')}
          </button>
          <p className="feat-hint" style={{ margin: 0 }}>
            Reference calculation only — this does NOT cap the miner. It records your
            safe circuit ceiling in the dashboard so you can size a real power/efficiency
            target in tuning.
          </p>
        </div>
      </div>
    </div>
  );
}
