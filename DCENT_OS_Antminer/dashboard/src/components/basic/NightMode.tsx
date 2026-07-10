import React, { useState, useEffect } from 'react';
import { useMinerStore } from '../../store/miner';
import { api } from '../../api/client';
import { glossaryText } from '../../utils/glossary';

export function NightMode() {
  const nightMode = useMinerStore(s => s.nightMode);
  const setNightMode = useMinerStore(s => s.setNightMode);
  const addToast = useMinerStore(s => s.addToast);

  const [enabled, setEnabled] = useState(nightMode?.enabled ?? false);
  const [startHour, setStartHour] = useState(nightMode?.start_hour ?? 22);
  const [endHour, setEndHour] = useState(nightMode?.end_hour ?? 7);
  const [reductionPct, setReductionPct] = useState(nightMode?.power_reduction_pct ?? 50);

  // Sync from store when API data arrives
  useEffect(() => {
    if (nightMode) {
      setEnabled(nightMode.enabled);
      setStartHour(nightMode.start_hour);
      setEndHour(nightMode.end_hour);
      setReductionPct(nightMode.power_reduction_pct);
    }
  }, [nightMode]);

  const commit = async (nextEnabled: boolean) => {
    try {
      await api.setNightMode({
        enabled: nextEnabled,
        start_hour: startHour,
        end_hour: endHour,
        power_reduction_pct: reductionPct,
      });
      setNightMode({ enabled: nextEnabled, start_hour: startHour, end_hour: endHour, max_fan_pwm: nightMode?.max_fan_pwm ?? 30, power_reduction_pct: reductionPct, active: nightMode?.active ?? false });
    } catch {
      addToast('Failed to save night mode settings', 'error');
      throw new Error('night-mode commit failed');
    }
  };

  const handleSave = async () => {
    await commit(enabled).catch(() => {});
  };

  // HEATER-4: the toggle must be BIDIRECTIONAL. Previously it only flipped
  // local state, and the "Save" button (which commits to the daemon) lives
  // inside the `enabled &&` body — so toggling OFF hid the only control that
  // could persist the change and night mode could be ENABLED but never
  // DISABLED. Now every toggle commits to the server immediately (optimistic,
  // reverted on failure) and reflects the committed state from the store.
  const handleToggle = async () => {
    const next = !enabled;
    setEnabled(next);
    try {
      await commit(next);
    } catch {
      setEnabled(!next);
    }
  };

  const hourOptions = Array.from({ length: 24 }, (_, i) => i);
  const formatHour = (h: number) => `${h.toString().padStart(2, '0')}:00`;

  return (
    <div className="night-mode-card">
      <div className="night-mode-head">
        <div>
          <div
            className="night-mode-title"
            data-tooltip={glossaryText('cut_hash_before_noise')}
          >
            Night Mode
          </div>
          <div className="night-mode-subtitle">Reduce power during sleeping hours</div>
        </div>
        <div className="night-mode-head-actions">
          {nightMode?.active && (
            <span
              className="night-mode-active-pill"
              data-tooltip={glossaryText('night_mode_behaviour')}
            >
              Active
            </span>
          )}
          <button
            type="button"
            role="switch"
            aria-checked={enabled ? 'true' : 'false'}
            aria-label="Night mode"
            tabIndex={0}
            className={`toggle-switch night-mode-toggle${enabled ? ' on' : ''}`}
            onClick={() => { void handleToggle(); }}
            onKeyDown={(e) => {
              if (e.key === ' ' || e.key === 'Enter') {
                e.preventDefault();
                void handleToggle();
              }
            }}
          >
            <div className="thumb" aria-hidden="true" />
          </button>
        </div>
      </div>

      {enabled && (
        <div className="night-mode-body">
          <div className="night-mode-hours">
            <label className="night-mode-field">
              <div className="night-mode-field-label">Start</div>
              <select
                className="night-mode-select"
                value={startHour}
                onChange={e => setStartHour(Number(e.target.value))}
              >
                {hourOptions.map(h => <option key={h} value={h}>{formatHour(h)}</option>)}
              </select>
            </label>
            <label className="night-mode-field">
              <div className="night-mode-field-label">End</div>
              <select
                className="night-mode-select"
                value={endHour}
                onChange={e => setEndHour(Number(e.target.value))}
              >
                {hourOptions.map(h => <option key={h} value={h}>{formatHour(h)}</option>)}
              </select>
            </label>
          </div>

          <div>
            <div className="night-mode-slider-head">
              <label
                htmlFor="night-mode-reduction"
                data-tooltip={glossaryText('night_mode_behaviour')}
              >
                Power Reduction
              </label>
              <span aria-hidden="true" className="night-mode-slider-value">{reductionPct}%</span>
            </div>
            <input
              id="night-mode-reduction"
              className="night-mode-range"
              type="range"
              min={10}
              max={90}
              step={5}
              value={reductionPct}
              onChange={e => setReductionPct(Number(e.target.value))}
              aria-label={`Power reduction: ${reductionPct}%`}
              aria-valuemin={10}
              aria-valuemax={90}
              aria-valuenow={reductionPct}
              aria-valuetext={`${reductionPct}%`}
            />
            <div className="night-mode-range-scale">
              <span>10%</span>
              <span>90%</span>
            </div>
          </div>

          <button
            type="button"
            className="settings-save-btn night-mode-save"
            onClick={handleSave}
          >
            Save Night Mode
          </button>
        </div>
      )}
    </div>
  );
}
