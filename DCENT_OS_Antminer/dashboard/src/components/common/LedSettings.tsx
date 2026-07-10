import React, { useState, useEffect } from 'react';
import api from '../../api/client';
import type { LedPatternInfo, LedConfigResponse, LedConfigUpdateRequest } from '../../api/types';
import { SectionSkeleton } from './skeletons/SectionSkeleton';

/**
 * LED configuration panel for settings pages.
 * Allows configuring heartbeat timing, locate pattern, share flash, etc.
 */
export function LedSettings() {
  const [config, setConfig] = useState<LedConfigResponse | null>(null);
  const [patterns, setPatterns] = useState<LedPatternInfo[]>([]);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    Promise.all([
      api.getLedConfig(),
      api.getLedPatterns(),
    ]).then(([cfg, pats]) => {
      setConfig(cfg);
      setPatterns(pats.locate_patterns ?? pats.patterns ?? []);
    }).catch(() => {});
  }, []);

  const save = async (partial: LedConfigUpdateRequest) => {
    setSaving(true);
    try {
      await api.updateLedConfig(partial);
      setConfig(prev => prev ? { ...prev, ...partial } : prev);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch {}
    setSaving(false);
  };

  if (!config) {
    return (
      <div className="led-settings-panel" data-testid="led-loading">
        <SectionSkeleton rows={4} data-testid="skeleton-led-settings" />
      </div>
    );
  }

  return (
    <div className="led-settings-panel">
      <div className="led-header">
        LED Settings
        {saved && <span className="led-saved-badge" aria-live="polite">Saved</span>}
      </div>

      {/* Enable/Disable */}
      <div className="led-section">
        <label className="led-label-row">
          <span>LED indicators enabled</span>
          <input
            type="checkbox"
            checked={config.enabled}
            onChange={e => save({ enabled: e.target.checked })}
          />
        </label>
      </div>

      {/* Find My Miner Pattern */}
      <div className="led-section">
        <label htmlFor="led-locate-pattern" className="led-select-label">Find My Miner Pattern</label>
        <select
          id="led-locate-pattern"
          value={config.locate_pattern}
          onChange={e => save({ locate_pattern: e.target.value })}
          className="led-select"
        >
          {patterns.map(p => (
            <option key={p.id} value={p.id}>{p.name} — {p.description}</option>
          ))}
        </select>
      </div>

      {/* Locate Duration */}
      <div className="led-section">
        <label htmlFor="led-locate-duration" className="led-label-row">
          <span>Locate duration</span>
          <span aria-hidden="true" className="led-range-value">{config.locate_duration_s}s</span>
        </label>
        <input
          id="led-locate-duration"
          type="range"
          min={10} max={60} step={5}
          value={config.locate_duration_s}
          onChange={e => {
            const v = parseInt(e.target.value);
            setConfig(prev => prev ? { ...prev, locate_duration_s: v } : prev);
          }}
          onMouseUp={e => save({ locate_duration_s: parseInt((e.target as HTMLInputElement).value) })}
          onTouchEnd={e => save({ locate_duration_s: parseInt((e.target as HTMLInputElement).value) })}
          className="led-range-input"
          aria-label={`Locate duration: ${config.locate_duration_s} seconds`}
          aria-valuemin={10}
          aria-valuemax={60}
          aria-valuenow={config.locate_duration_s}
          aria-valuetext={`${config.locate_duration_s} seconds`}
        />
      </div>

      {/* Heartbeat Timing */}
      <div className="led-section">
        <div className="led-subsection-title">Mining Heartbeat</div>
        <div className="led-desc">Controls how the green LED blinks during normal mining. Speeds up with temperature.</div>
        <div className="led-input-row">
          <div className="led-input-col">
            <label className="led-input-label">On (ms)</label>
            <input
              type="number"
              min={30} max={500} step={10}
              value={config.heartbeat_on_ms}
              aria-label="Heartbeat LED on duration in milliseconds"
              onChange={e => {
                const v = parseInt(e.target.value) || 100;
                setConfig(prev => prev ? { ...prev, heartbeat_on_ms: v } : prev);
              }}
              onBlur={e => save({ heartbeat_on_ms: parseInt(e.target.value) || 100 })}
              className="led-input"
            />
          </div>
          <div className="led-input-col">
            <label className="led-input-label">Off (ms)</label>
            <input
              type="number"
              min={50} max={2000} step={50}
              value={config.heartbeat_off_ms}
              aria-label="Heartbeat LED off duration in milliseconds"
              onChange={e => {
                const v = parseInt(e.target.value) || 900;
                setConfig(prev => prev ? { ...prev, heartbeat_off_ms: v } : prev);
              }}
              onBlur={e => save({ heartbeat_off_ms: parseInt(e.target.value) || 900 })}
              className="led-input"
            />
          </div>
        </div>
      </div>

      {/* Notifications */}
      <div className="led-section">
        <div className="led-subsection-title">Notifications</div>
        <label className="led-label-row">
          <div>
            <span>Flash green on accepted share</span>
            <div className="led-desc">Brief green blink when pool accepts your work</div>
          </div>
          <input
            type="checkbox"
            checked={config.flash_on_accepted_share}
            onChange={e => save({ flash_on_accepted_share: e.target.checked })}
          />
        </label>
        <label className="led-label-row">
          <div>
            <span>Flash red on rejected share</span>
            <div className="led-desc">Brief red blink when pool rejects a share</div>
          </div>
          <input
            type="checkbox"
            checked={config.flash_on_rejected_share}
            onChange={e => save({ flash_on_rejected_share: e.target.checked })}
          />
        </label>
        <label className="led-label-row">
          <div>
            <span>Celebrate lucky shares</span>
            <div className="led-desc">Flash both LEDs when a share is 10x+ above target difficulty</div>
          </div>
          <input
            type="checkbox"
            checked={config.celebration_on_lucky_share}
            onChange={e => save({ celebration_on_lucky_share: e.target.checked })}
          />
        </label>
        <label className="led-label-row">
          <div>
            <span>Chain online blink codes</span>
            <div className="led-desc">Flash green N times when chain N comes online during boot</div>
          </div>
          <input
            type="checkbox"
            checked={config.chain_status_blink_codes}
            onChange={e => save({ chain_status_blink_codes: e.target.checked })}
          />
        </label>
      </div>

      {/* Night Mode */}
      <div className="led-section">
        <label className="led-label-row">
          <div>
            <span>Disable LEDs during night mode</span>
            <div className="led-desc">Uses the same schedule as thermal night mode. Diagnostic LEDs (D7/D8) stay on.</div>
          </div>
          <input
            type="checkbox"
            checked={config.night_mode_disable}
            onChange={e => save({ night_mode_disable: e.target.checked })}
          />
        </label>
      </div>
    </div>
  );
}
