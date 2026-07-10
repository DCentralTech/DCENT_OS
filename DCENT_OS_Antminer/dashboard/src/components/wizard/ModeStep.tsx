// DCENT_OS Setup Wizard — Mode step.
//
// Structural recreation of the kit `ModeStep` (ui_kits/wizard/Wizard.jsx):
// the 3 mode cards each with a MINI MODE-PREVIEW (the ember-orb / scanline /
// phosphor-terminal thumbnails), heading + sub + "when" line, active glow.
//
// Real wiring preserved: production OperatingMode ('heater'|'standard'|
// 'hacker') drives api.setupMode. The kit's preview classes are
// basic/standard/hacker — heater maps to the kit "basic" ember preview.

import React from 'react';
import type { OperatingMode } from '../../api/types';

interface ModeStepProps {
  value: OperatingMode | null;
  onChange: (mode: OperatingMode) => void;
}

interface ModeCard {
  id: OperatingMode;
  preview: 'basic' | 'standard' | 'hacker';
  l: string;
  sub: string;
  when: string;
  bullets: string[];
}

const MODES: ModeCard[] = [
  {
    id: 'heater',
    preview: 'basic',
    l: 'Basic / Heating',
    sub: 'Thermostat-first surface for home miners.',
    when: 'Best for people heating a room.',
    bullets: ['Thermostat-style interface', 'BTU output & sats tracker', 'Quiet night mode'],
  },
  {
    id: 'standard',
    preview: 'standard',
    l: 'Standard / Mining',
    sub: 'Full mining dashboard with all telemetry.',
    when: 'Best for daily operators.',
    bullets: ['Full hashrate charts', 'Pool management', 'Tuning profiles'],
  },
  {
    id: 'hacker',
    preview: 'hacker',
    l: 'Advanced / Hacking',
    sub: 'TUI-feel power-user workstation.',
    when: 'Best for IT folks and OG miners.',
    bullets: ['Raw FPGA access', 'ASIC commands', 'Voltage control'],
  },
];

export function ModeStep({ value, onChange }: ModeStepProps) {
  return (
    <div className="wiz-step-body wizard-step-pane">
      <h2 className="wiz-h2">Pick a default mode</h2>
      <p className="wiz-lede">You can switch modes anytime from the sidebar.</p>

      <div className="wiz-mode-grid" role="radiogroup" aria-label="Operating mode">
        {MODES.map(m => {
          const isActive = value === m.id;
          return (
            <button
              key={m.id}
              type="button"
              role="radio"
              aria-checked={isActive}
              tabIndex={isActive || value === null ? 0 : -1}
              className={`wiz-mode-card${isActive ? ' active' : ''}${isActive ? ' wizard-tile-selected-halo' : ''}`}
              onClick={() => onChange(m.id)}
            >
              {m.id === 'standard' && <div className="wiz-mode-rec">Recommended</div>}
              <div className={`wiz-mode-preview wiz-mode-preview-${m.preview}`}>
                <div className="wiz-mode-preview-inner" />
              </div>
              <h3>{m.l}</h3>
              <p>{m.sub}</p>
              <ul>
                {m.bullets.map((b, i) => (
                  <li key={i}>{b}</li>
                ))}
              </ul>
              <small>{m.when}</small>
            </button>
          );
        })}
      </div>

      <div className="wiz-info">
        <strong>Not sure?</strong> Standard is the best fit for most operators — it
        has the full dashboard and you can switch modes anytime in Settings.
      </div>
    </div>
  );
}
