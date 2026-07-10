// Immersion Cooling Mode — Hides fan controls, adjusts thermal for liquid cooling
// Feature no competitor has: first-class immersion support in miner firmware UI

import React, { useState } from 'react';
import type { CoolantType, ImmersionConfig } from '../../api/feature-types';
import { useTranslation } from '../../i18n/i18n';
import { useMinerStore } from '../../store/miner';
import { InfoDot } from '../common/Tooltip';

const COOLANT_OPTIONS: { value: CoolantType; label: string; maxTemp: number; description: string }[] = [
  { value: 'mineral-oil', label: 'Mineral Oil', maxTemp: 85, description: 'Common, affordable. Good thermal transfer.' },
  { value: 'dielectric-fluid', label: 'Dielectric Fluid', maxTemp: 90, description: '3M Novec / Fluorinert. Premium performance.' },
  { value: 'engineered-fluid', label: 'Engineered Fluid', maxTemp: 95, description: 'BitCool / ThermaSafe. Mining-specific.' },
  { value: 'custom', label: 'Custom', maxTemp: 80, description: 'Custom coolant — set temperature limits manually.' },
];

// There is NO backend immersion config in dcentrald yet, so this view persists
// the operator's choices to the browser only (localStorage). The copy is honest
// about that — we do not claim a daemon-apply that cannot happen.
const IMMERSION_STORAGE_KEY = 'dcentos-immersion-config';

const DEFAULT_IMMERSION_CONFIG: ImmersionConfig = {
  enabled: false,
  coolantType: 'mineral-oil',
  maxChipTempC: 85,
  inletTempC: null,
  outletTempC: null,
  flowRateLpm: null,
};

function loadImmersionConfig(): ImmersionConfig {
  try {
    const raw = localStorage.getItem(IMMERSION_STORAGE_KEY);
    if (raw) {
      return { ...DEFAULT_IMMERSION_CONFIG, ...(JSON.parse(raw) as Partial<ImmersionConfig>) };
    }
  } catch { /* ignore malformed / unavailable storage */ }
  return { ...DEFAULT_IMMERSION_CONFIG };
}

export function ImmersionMode() {
  const { t } = useTranslation();
  const addAlert = useMinerStore(s => s.addAlert);
  const chains = useMinerStore(s => s.status?.chains ?? []);

  const [config, setConfig] = useState<ImmersionConfig>(loadImmersionConfig);

  const selectedCoolant = COOLANT_OPTIONS.find(c => c.value === config.coolantType);

  const update = (partial: Partial<ImmersionConfig>) => {
    setConfig(prev => {
      const next = { ...prev, ...partial };
      // Auto-adjust max temp when coolant changes
      if (partial.coolantType) {
        const coolant = COOLANT_OPTIONS.find(c => c.value === partial.coolantType);
        if (coolant) next.maxChipTempC = coolant.maxTemp;
      }
      return next;
    });
  };

  const handleSave = () => {
    // There is NO backend immersion config in dcentrald yet, so this persists
    // to the browser only (localStorage) and the copy says exactly that. We do
    // NOT tell the operator to "restart the daemon to apply" — nothing on the
    // miner reads these values, so a restart would change nothing.
    let persisted = true;
    try {
      localStorage.setItem(IMMERSION_STORAGE_KEY, JSON.stringify(config));
    } catch {
      persisted = false;
    }
    if (!persisted) {
      addAlert('warning', 'Could not save immersion settings — browser storage is unavailable.');
      return;
    }
    addAlert('info', config.enabled
      ? 'Immersion settings saved in this browser only — not yet applied to the miner.'
      : 'Immersion settings saved in this browser only (disabled) — not yet applied to the miner.');
  };

  // Calculate delta T if both inlet and outlet are provided
  const deltaT = (config.inletTempC !== null && config.outletTempC !== null)
    ? config.outletTempC - config.inletTempC
    : null;

  // Get current chip temps
  const chipTemps = chains.filter(c => c.temp_c > 0).map(c => c.temp_c);
  const maxChipTemp = chipTemps.length > 0 ? Math.max(...chipTemps) : 0;
  const avgChipTemp = chipTemps.length > 0
    ? chipTemps.reduce((s, t) => s + t, 0) / chipTemps.length
    : 0;

  const immersionEnabledLabelId = 'immersion-enabled-label';
  const maxChipTempId = 'immersion-max-chip-temp';
  const maxChipTempHintId = 'immersion-max-chip-temp-hint';
  const inletTempId = 'immersion-inlet-temp';
  const outletTempId = 'immersion-outlet-temp';
  const flowRateId = 'immersion-flow-rate';

  return (
    <div className="feat-page">
      <div className="feat-header">
        <h2 className="feat-title feat-title-blue">
          {t('immersion.title')}
          <InfoDot
            placement="bottom"
            label="What immersion mode does"
            content={
              <>
                For miners submerged in dielectric coolant instead of air-cooled.
                Turning this on hides the fan controls (there are no fans in the
                tank) and re-targets the thermal loop for liquid cooling, which
                runs hotter-but-safer and dead silent — ideal for a quiet home
                heater loop.
              </>
            }
          />
        </h2>
        <p className="feat-subtitle">{t('immersion.subtitle')}</p>
      </div>

      {/* Enable toggle */}
      <div className="feat-card">
        <label className="feat-toggle-row">
          <span className="feat-toggle-label" id={immersionEnabledLabelId}>{t('common.enabled')}</span>
          <button
            type="button"
            role="switch"
            aria-checked={config.enabled}
            aria-labelledby={immersionEnabledLabelId}
            className={`feat-toggle ${config.enabled ? 'active' : ''}`}
            onClick={() => update({ enabled: !config.enabled })}
          >
            <span className="feat-toggle-knob" />
          </button>
        </label>

        {config.enabled && (
          <div className="feat-immersion-notice">
            Planning workspace only — these immersion settings are saved in this
            browser and are not yet applied to the miner. The daemon does not yet
            hide fan controls or re-target thermal thresholds for liquid cooling;
            use these values to plan your loop and read the live coolant metrics below.
          </div>
        )}
      </div>

      {config.enabled && (
        <>
          {/* Coolant selection */}
          <div className="feat-card">
            <h3 className="feat-card-title">{t('immersion.coolantType')}</h3>
            <div className="feat-coolant-grid">
              {COOLANT_OPTIONS.map(coolant => (
                <button
                  type="button"
                  key={coolant.value}
                  className={`feat-coolant-card ${config.coolantType === coolant.value ? 'active' : ''}`}
                  aria-pressed={config.coolantType === coolant.value}
                  aria-label={`${coolant.label}. ${coolant.description} Max ${coolant.maxTemp}C.`}
                  onClick={() => update({ coolantType: coolant.value })}
                >
                  <div className="feat-coolant-name">{coolant.label}</div>
                  <div className="feat-coolant-desc">{coolant.description}</div>
                  <div className="feat-coolant-temp">Max: {coolant.maxTemp}C</div>
                </button>
              ))}
            </div>
          </div>

          {/* Temperature config */}
          <div className="feat-card">
            <h3 className="feat-card-title">Thermal Configuration</h3>
            <div className="feat-form-grid">
              <div className="feat-input-group">
                <label className="feat-label" htmlFor={maxChipTempId}>{t('immersion.maxChipTemp')}</label>
                <div className="feat-safety-display">
                  <input
                    id={maxChipTempId}
                    type="range"
                    min="60"
                    max="100"
                    value={config.maxChipTempC}
                    onChange={e => update({ maxChipTempC: Number(e.target.value) })}
                    className="feat-range"
                    aria-describedby={selectedCoolant ? maxChipTempHintId : undefined}
                  />
                  <span className="feat-range-value" aria-live="polite">{config.maxChipTempC}C</span>
                </div>
                {selectedCoolant && (
                  <div className="feat-hint" id={maxChipTempHintId}>
                    Recommended max for {selectedCoolant.label}: {selectedCoolant.maxTemp}C
                  </div>
                )}
              </div>

              <div className="feat-input-group">
                <label className="feat-label" htmlFor={inletTempId}>{t('immersion.inletTemp')}</label>
                <input
                  id={inletTempId}
                  type="number"
                  min="0"
                  max="80"
                  value={config.inletTempC ?? ''}
                  onChange={e => update({ inletTempC: e.target.value ? Number(e.target.value) : null })}
                  placeholder="Optional"
                  className="feat-input"
                />
              </div>

              <div className="feat-input-group">
                <label className="feat-label" htmlFor={outletTempId}>{t('immersion.outletTemp')}</label>
                <input
                  id={outletTempId}
                  type="number"
                  min="0"
                  max="100"
                  value={config.outletTempC ?? ''}
                  onChange={e => update({ outletTempC: e.target.value ? Number(e.target.value) : null })}
                  placeholder="Optional"
                  className="feat-input"
                />
              </div>

              <div className="feat-input-group">
                <label className="feat-label" htmlFor={flowRateId}>{t('immersion.flowRate')}</label>
                <input
                  id={flowRateId}
                  type="number"
                  min="0"
                  step="0.1"
                  value={config.flowRateLpm ?? ''}
                  onChange={e => update({ flowRateLpm: e.target.value ? Number(e.target.value) : null })}
                  placeholder="Optional"
                  className="feat-input"
                />
              </div>
            </div>
          </div>

          {/* Live immersion metrics */}
          <div className="feat-card">
            <h3 className="feat-card-title">Immersion Metrics</h3>
            <div className="feat-immersion-metrics">
              <div className="feat-metric-card feat-metric-blue">
                <div className="feat-metric-label">Avg Chip Temp</div>
                <div className="feat-metric-value">
                  {avgChipTemp > 0 ? `${avgChipTemp.toFixed(1)}C` : 'N/A'}
                </div>
              </div>
              <div className="feat-metric-card feat-metric-blue">
                <div className="feat-metric-label">Max Chip Temp</div>
                <div className="feat-metric-value" style={{
                  color: maxChipTemp > config.maxChipTempC ? 'var(--feat-red)' : 'var(--feat-blue)',
                }}>
                  {maxChipTemp > 0 ? `${maxChipTemp.toFixed(1)}C` : 'N/A'}
                </div>
              </div>
              {deltaT !== null && (
                <div className="feat-metric-card feat-metric-blue">
                  <div className="feat-metric-label">{t('immersion.deltaT')}</div>
                  <div className="feat-metric-value">{deltaT.toFixed(1)}C</div>
                </div>
              )}
              {config.flowRateLpm !== null && (
                <div className="feat-metric-card feat-metric-blue">
                  <div className="feat-metric-label">{t('immersion.flowRate')}</div>
                  <div className="feat-metric-value">{config.flowRateLpm} L/min</div>
                </div>
              )}
            </div>
          </div>

          {/* Save */}
          <div className="feat-actions">
            <button type="button" className="feat-btn feat-btn-primary" onClick={handleSave}>
              {t('common.save')}
            </button>
          </div>
        </>
      )}
    </div>
  );
}
