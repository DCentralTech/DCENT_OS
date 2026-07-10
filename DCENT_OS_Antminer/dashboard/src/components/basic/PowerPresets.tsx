import React, { useState } from 'react';
import { useMinerStore } from '../../store/miner';
import { useHeaterPresets } from '../../hooks/useHeaterPresets';
import { api } from '../../api/client';
import { wattsToBtu } from '../../utils/thermal';
import { glossaryText } from '../../utils/glossary';

// Plain-language "what does this preset feel like" hover, by warm-tone bucket.
// Reinforces the quiet-home posture without ever overstating noise.
const PRESET_TONE_HELP: Record<string, string> = {
  eco: 'Lowest power - least heat and least cost. Acoustic result still needs RPM proof.',
  silent: 'Lowest fan-cap request. Less heat; verify noise with live RPM.',
  quiet: 'A calm middle setting with the home fan cap requested.',
  balanced: 'A balanced amount of heat and Bitcoin work for everyday room heating.',
  max: 'Most heat and the most Bitcoin work. Warmest room, highest power draw and likely noise.',
};

// Flame intensity: 1-3 based on power level
function FlameIcon({ intensity }: { intensity: number }) {
  const color = intensity >= 3 ? '#EF4444' : intensity >= 2 ? '#F7931A' : '#F59E0B';
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill={color} stroke="none" opacity={0.8}>
      <path d="M8.5 14.5A2.5 2.5 0 0 0 11 12c0-1.38-.5-2-1-3-1.072-2.143-.224-4.054 2-6 .5 2.5 2 4.9 4 6.5 2 1.6 3 3.5 3 5.5a7 7 0 1 1-14 0c0-1.153.433-2.294 1-3a2.5 2.5 0 0 0 2.5 2.5z" />
    </svg>
  );
}

function flameIntensity(watts: number): number {
  if (watts <= 400) return 1;
  if (watts <= 900) return 2;
  return 3;
}

// Wall watts estimate: ~13.6% PSU overhead (board watts / 0.88)
function estimateWallWatts(boardWatts: number): number {
  return Math.round(boardWatts / 0.88);
}

// Map preset name to a warm-palette tone bucket consumed by basic.css
// via the `data-preset-tone` attribute. Falls back to 'balanced' for
// unknown preset names so the warm orange accent still renders.
function presetTone(name: string): 'eco' | 'quiet' | 'balanced' | 'max' | 'silent' {
  const lower = name.toLowerCase();
  if (lower.includes('eco')) return 'eco';
  if (lower.includes('silent')) return 'silent';
  if (lower.includes('quiet')) return 'quiet';
  if (lower.includes('max')) return 'max';
  return 'balanced';
}

export function PowerPresets() {
  const currentPreset = useMinerStore(s => s.heaterStatus?.preset ?? '');
  const presets = useHeaterPresets();
  // When the API never delivered presets, useHeaterPresets() transparently
  // falls back to the built-in Quiet/Balanced/Max set. Detect that here so we
  // can show a quiet "using defaults" note — the grid still renders fully, it
  // never blanks.
  const usingDefaultPresets = useMinerStore(s => s.heaterPresets.length === 0);
  const presetScope = useMinerStore(s => s.heaterPresetScope);
  const addToast = useMinerStore(s => s.addToast);
  const [applying, setApplying] = useState(false);

  const handleSelect = async (name: string) => {
    try {
      setApplying(true);
      await api.setHeaterTarget({ preset: name });
    } catch {
      addToast('Failed to apply preset', 'error');
    } finally {
      setApplying(false);
    }
  };

  // Check if the current preset matches one of the 3 presets; if not, it's "Custom"
  const isCustom = currentPreset !== '' && !presets.some(p => p.name === currentPreset);

  return (
    <div className="presets-section">
      <div className="presets-grid" style={{ gridTemplateColumns: 'repeat(3, 1fr)' }}>
        {presets.map(p => {
          const isActive = currentPreset === p.name;
          const wallW = estimateWallWatts(p.watts);
          const btu = p.btu_h > 0 ? p.btu_h : wattsToBtu(wallW);

          const tone = presetTone(p.name);
          return (
            <button
              key={p.name}
              className={`glass-card preset-btn${isActive ? ' active' : ''}${applying ? ' disabled' : ''}`}
              data-preset-tone={tone}
              data-tooltip={PRESET_TONE_HELP[tone] ?? PRESET_TONE_HELP.balanced}
              onClick={() => handleSelect(p.name)}
              disabled={applying}
              aria-pressed={isActive}
              aria-label={`Set heater to ${p.display_name || p.name} preset, approximately ${p.watts} watts`}
            >
              <div className="preset-btn-header">
                <FlameIcon intensity={flameIntensity(p.watts)} />
                <span className="preset-name">
                  {p.display_name || (p.name === 'quiet' ? 'Quiet' : p.name === 'balanced' ? 'Balanced' : p.name === 'max' ? 'Max Heat' : p.name.charAt(0).toUpperCase() + p.name.slice(1))}
                </span>
              </div>
              <span className="watts">{p.watts}W</span>
              <div className="preset-specs">
                <span className="preset-btu">{btu.toLocaleString()} BTU/h</span>
              </div>
            </button>
          );
        })}
      </div>
      {isCustom && (
        <div className="presets-note presets-note--italic">
          Custom power ({currentPreset})
        </div>
      )}
      {usingDefaultPresets && (
        <div className="presets-note presets-note--italic">
          Showing default presets — live preset data unavailable
        </div>
      )}
      {presetScope && presetScope.universal === false && (
        <div className="presets-note">
          Preset estimates shown for {presetScope.label}
        </div>
      )}
      <div
        className="presets-quiet-note"
        data-tooltip={glossaryText('cut_hash_before_noise')}
      >
        Higher presets run warmer. DCENT_OS cuts hash power before raising fan
        noise. Acoustic claims require fan RPM feedback.
      </div>
    </div>
  );
}
