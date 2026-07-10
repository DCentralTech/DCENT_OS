// Demand Response readiness. Runtime control requires a real backend policy source.

import React, { useState } from 'react';
import type { GridOperator, DemandResponseConfig } from '../../api/feature-types';
import { useTranslation } from '../../i18n/i18n';
import { useMinerStore } from '../../store/miner';
import { InfoDot } from '../common/Tooltip';

const GRID_OPERATORS: { value: GridOperator; label: string; region: string }[] = [
  { value: 'ercot', label: 'ERCOT', region: 'Texas' },
  { value: 'caiso', label: 'CAISO', region: 'California' },
  { value: 'pjm', label: 'PJM', region: 'Mid-Atlantic / Midwest' },
  { value: 'nyiso', label: 'NYISO', region: 'New York' },
  { value: 'miso', label: 'MISO', region: 'Central US' },
  { value: 'hydro-quebec', label: 'Hydro-Qu\u00E9bec', region: 'Qu\u00E9bec' },
  { value: 'manual', label: 'Manual / Other', region: 'Custom API' },
];

export function DemandResponse() {
  const { t } = useTranslation();
  const addAlert = useMinerStore(s => s.addAlert);

  const [config, setConfig] = useState<DemandResponseConfig>({
    enabled: false,
    gridOperator: 'hydro-quebec',
    curtailmentThresholdCentsKwh: 15,
    negativePriceMining: true,
    apiEndpoint: '',
  });

  const update = (partial: Partial<DemandResponseConfig>) => {
    setConfig(prev => ({ ...prev, ...partial }));
  };

  const handleSave = () => {
    addAlert('info', 'Demand response draft updated locally. No miner state changed.');
  };

  const selectedOperator = GRID_OPERATORS.find(o => o.value === config.gridOperator);

  const demandEnabledLabelId = 'demand-enabled-label';
  const gridOperatorId = 'demand-grid-operator';
  const apiEndpointId = 'demand-api-endpoint';
  const curtailmentThresholdId = 'demand-curtailment-threshold';
  const negativePriceLabelId = 'demand-negative-price-label';

  return (
    <div className="feat-page">
      <div className="feat-header">
        <h2 className="feat-title feat-title-blue">
          {t('demand.title')}
          <InfoDot
            placement="bottom"
            label="What demand response does"
            content={
              <>
                Demand response planning helps define when a miner should reduce
                load during grid stress or negative-price windows. Runtime
                control needs a real backend policy source - this screen drafts
                the policy only.
              </>
            }
          />
        </h2>
        <p className="feat-subtitle">{t('demand.subtitle')}</p>
      </div>

      {/* Enable toggle */}
      <div className="feat-card">
        <label className="feat-toggle-row">
          <span className="feat-toggle-label" id={demandEnabledLabelId}>Draft readiness settings</span>
          <button
            type="button"
            role="switch"
            aria-checked={config.enabled}
            aria-labelledby={demandEnabledLabelId}
            className={`feat-toggle ${config.enabled ? 'active' : ''}`}
            onClick={() => update({ enabled: !config.enabled })}
          >
            <span className="feat-toggle-knob" />
          </button>
        </label>
        <div className="feat-hint">
          This page is a planning surface only. It does not send curtailment, sleep, wake, fan, voltage, frequency, or pool commands.
        </div>
      </div>

      {config.enabled && (
        <>
          {/* Current status */}
          <div className="feat-card">
            <h3 className="feat-card-title">Runtime Status</h3>
            <div className="feat-demand-status">
              <div className="feat-demand-price">
                <div className="feat-demand-price-value" style={{ color: 'var(--feat-yellow)' }}>
                  Unavailable
                </div>
                <div className="feat-demand-price-unit">No live price source</div>
              </div>
              <div className="feat-demand-signal">
                <span
                  className="feat-demand-badge"
                  style={{
                    background: 'var(--feat-yellow)',
                    color: '#000',
                  }}
                >
                  READINESS ONLY
                </span>
                <span className="feat-demand-curtailed">No curtailment command is sent from this page</span>
              </div>
              <div className="feat-demand-stats">
                <div className="feat-demand-stat">
                  <span className="feat-demand-stat-label">Runtime control</span>
                  <span className="feat-demand-stat-value">In development</span>
                </div>
                <div className="feat-demand-stat">
                  <span className="feat-demand-stat-label">Price source</span>
                  <span className="feat-demand-stat-value">Not connected</span>
                </div>
                <div className="feat-demand-stat">
                  <span className="feat-demand-stat-label">Revenue impact</span>
                  <span className="feat-demand-stat-value">Not calculated</span>
                </div>
                <div className="feat-demand-stat">
                  <span className="feat-demand-stat-label">Negative price hours</span>
                  <span className="feat-demand-stat-value feat-value-blue">Unavailable</span>
                </div>
              </div>
            </div>
          </div>

          {/* Configuration */}
          <div className="feat-card">
            <h3 className="feat-card-title">{t('demand.gridOperator')}</h3>
            <div className="feat-form-grid">
              <div className="feat-input-group feat-span-2">
                <label className="feat-label" htmlFor={gridOperatorId}>{t('demand.gridOperator')}</label>
                <select
                  id={gridOperatorId}
                  value={config.gridOperator}
                  onChange={e => update({ gridOperator: e.target.value as GridOperator })}
                  className="feat-input"
                >
                  {GRID_OPERATORS.map(op => (
                    <option key={op.value} value={op.value}>
                      {op.label} ({op.region})
                    </option>
                  ))}
                </select>
                {selectedOperator && (
                  <div className="feat-hint">Region: {selectedOperator.region}</div>
                )}
              </div>

              {config.gridOperator === 'manual' && (
                <div className="feat-input-group feat-span-2">
                  <label className="feat-label" htmlFor={apiEndpointId}>API Endpoint</label>
                  <input
                    id={apiEndpointId}
                    type="url"
                    value={config.apiEndpoint}
                    onChange={e => update({ apiEndpoint: e.target.value })}
                    placeholder="https://prices.example.com/feed"
                    className="feat-input"
                  />
                </div>
              )}

              <div className="feat-input-group">
                <label className="feat-label" htmlFor={curtailmentThresholdId}>{t('demand.curtailmentThreshold')}</label>
                <input
                  id={curtailmentThresholdId}
                  type="number"
                  min="0"
                  step="1"
                  value={config.curtailmentThresholdCentsKwh}
                  onChange={e => update({ curtailmentThresholdCentsKwh: Number(e.target.value) })}
                  className="feat-input"
                />
                <div className="feat-hint">
                  Draft threshold for future policy design. It is not applied to mining, fan, voltage, or pool state.
                </div>
              </div>
            </div>

            {/* Negative price mining */}
            <label className="feat-toggle-row" style={{ marginTop: 16 }}>
              <div>
                <span className="feat-toggle-label" id={negativePriceLabelId}>{t('demand.negativePriceMining')}</span>
                <div className="feat-hint">
                  Planning flag only. DCENT_OS is not fetching grid prices or changing power from this page.
                </div>
              </div>
              <button
                type="button"
                role="switch"
                aria-checked={config.negativePriceMining}
                aria-labelledby={negativePriceLabelId}
                className={`feat-toggle ${config.negativePriceMining ? 'active' : ''}`}
                onClick={() => update({ negativePriceMining: !config.negativePriceMining })}
              >
                <span className="feat-toggle-knob" />
              </button>
            </label>

            {config.negativePriceMining && (
              <div className="feat-negative-price-badge">
                Draft only - no runtime power change
              </div>
            )}
          </div>

          {/* Save */}
          <div className="feat-actions">
            <button type="button" className="feat-btn feat-btn-primary" onClick={handleSave}>
              Acknowledge Draft
            </button>
          </div>
        </>
      )}
    </div>
  );
}
