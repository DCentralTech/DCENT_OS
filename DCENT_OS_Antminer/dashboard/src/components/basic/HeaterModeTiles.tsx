import React, { useState } from 'react';
import { useMinerStore } from '../../store/miner';
import { useHeaterPresets } from '../../hooks/useHeaterPresets';
import { api } from '../../api/client';
import { glossaryText } from '../../utils/glossary';

/**
 * Heater-mode side-column "Boost / Away / Quiet" tiles — emits the kit
 * `nest-mode-tiles` / `nest-mode-tile` grammar (styled by
 * handoff-skin-heater.css) so it visually matches the design kit's
 * HeaterMode.jsx `ModeTile` row.
 *
 * Every tile is wired to a REAL existing store/API action — the SAME ones
 * PowerPresets.tsx and NightMode.tsx already use. No invented endpoints, no
 * fabricated state:
 *
 *   - Boost  → `api.setHeaterTarget({ preset: <max-tier preset> })`
 *              (the highest-power preset from the real preset list — the same
 *               `setHeaterTarget` call PowerPresets uses). Active when the
 *               server-reported `heaterStatus.preset` IS that preset.
 *   - Away   → `api.setHeaterTarget({ preset: <eco-tier preset> })`
 *              (the lowest-power preset — still mines/earns, just quietly).
 *              Active when the reported preset IS that preset.
 *   - Quiet  → toggles REAL Night Mode via `api.setNightMode(...)` (the exact
 *              call NightMode.tsx makes). Active when `nightMode.enabled`.
 *
 * Honest active states: a tile only shows `active` when the live store
 * reflects that the action actually took effect — never optimistically.
 */

// Inline icons (kit HeaterMode.jsx: bolt / home / moon — 20x20, stroke 1.6).
const TileIcon = {
  bolt: (
    <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M13 2L3 14h9l-1 8 10-12h-9l1-8z" />
    </svg>
  ),
  home: (
    <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M3 9.5L12 3l9 6.5V21a1 1 0 0 1-1 1h-5v-6h-6v6H4a1 1 0 0 1-1-1V9.5z" />
    </svg>
  ),
  moon: (
    <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
    </svg>
  ),
};

function lower(s: string | null | undefined): string {
  return (s ?? '').toLowerCase();
}

export function HeaterModeTiles() {
  const presets = useHeaterPresets();
  const currentPreset = useMinerStore(s => s.heaterStatus?.preset ?? '');
  const nightMode = useMinerStore(s => s.nightMode);
  const setNightMode = useMinerStore(s => s.setNightMode);
  const addToast = useMinerStore(s => s.addToast);
  const [busy, setBusy] = useState<string | null>(null);

  // Resolve the real max-power and eco presets from the live preset list
  // (sorted by wattage). Falls back to the built-in defaults via
  // useHeaterPresets() so the tiles still resolve when the API never
  // delivered presets — never a fabricated preset name.
  const byWatts = [...presets].sort((a, b) => a.watts - b.watts);
  const ecoPreset = byWatts[0];
  const maxPreset = byWatts[byWatts.length - 1];

  const boostActive =
    !!maxPreset && lower(currentPreset) === lower(maxPreset.name);
  // Away only counts when there are at least two distinct tiers and the
  // active preset is the eco one (and it isn't the same preset as Boost).
  const awayActive =
    !!ecoPreset &&
    ecoPreset.name !== maxPreset?.name &&
    lower(currentPreset) === lower(ecoPreset.name);
  const quietActive = !!nightMode?.enabled;

  const applyPreset = async (label: string, name: string | undefined) => {
    if (!name) return;
    try {
      setBusy(label);
      await api.setHeaterTarget({ preset: name });
    } catch {
      addToast(`Failed to apply ${label}`, 'error');
    } finally {
      setBusy(null);
    }
  };

  const toggleQuiet = async () => {
    const next = !(nightMode?.enabled ?? false);
    try {
      setBusy('Quiet');
      await api.setNightMode({
        enabled: next,
        start_hour: nightMode?.start_hour ?? 22,
        end_hour: nightMode?.end_hour ?? 7,
        power_reduction_pct: nightMode?.power_reduction_pct ?? 50,
      });
      // Mirror NightMode.tsx: reflect the committed state into the store so
      // the active pill is honest (only after the API call resolved).
      setNightMode({
        enabled: next,
        start_hour: nightMode?.start_hour ?? 22,
        end_hour: nightMode?.end_hour ?? 7,
        max_fan_pwm: nightMode?.max_fan_pwm ?? 30,
        power_reduction_pct: nightMode?.power_reduction_pct ?? 50,
        active: nightMode?.active ?? false,
      });
    } catch {
      addToast('Failed to toggle Quiet (Night Mode)', 'error');
    } finally {
      setBusy(null);
    }
  };

  const ecoWatts = ecoPreset ? `Eco ${ecoPreset.watts}W` : 'Eco';
  const maxWatts = maxPreset ? `Max ${maxPreset.watts}W` : 'Max heat';

  return (
    <div
      className="nest-mode-tiles"
      role="group"
      aria-label="Heater quick modes"
    >
      <button
        type="button"
        className={`nest-mode-tile${boostActive ? ' active' : ''}`}
        onClick={() => applyPreset('Boost', maxPreset?.name)}
        disabled={busy !== null || !maxPreset}
        aria-pressed={boostActive}
        data-tooltip="Run the highest-power preset for the most heat and the most Bitcoin work. Warmest room, highest draw and noise."
      >
        <span className="nest-mode-icon">{TileIcon.bolt}</span>
        <span className="nest-mode-label">Boost</span>
        <span className="nest-mode-sub">{maxWatts}</span>
      </button>

      <button
        type="button"
        className={`nest-mode-tile${awayActive ? ' active' : ''}`}
        onClick={() => applyPreset('Away', ecoPreset?.name)}
        disabled={busy !== null || !ecoPreset || ecoPreset.name === maxPreset?.name}
        aria-pressed={awayActive}
        data-tooltip="Drop to the lowest-power preset while you're out. The miner keeps earning sats with lower heat and fan demand."
      >
        <span className="nest-mode-icon">{TileIcon.home}</span>
        <span className="nest-mode-label">Away</span>
        <span className="nest-mode-sub">{ecoWatts}</span>
      </button>

      <button
        type="button"
        className={`nest-mode-tile${quietActive ? ' active' : ''}`}
        onClick={toggleQuiet}
        disabled={busy !== null}
        aria-pressed={quietActive}
        data-tooltip={glossaryText('cut_hash_before_noise')}
      >
        <span className="nest-mode-icon">{TileIcon.moon}</span>
        <span className="nest-mode-label">Quiet</span>
        <span className="nest-mode-sub">{quietActive ? 'On' : 'Night mode'}</span>
      </button>
    </div>
  );
}
