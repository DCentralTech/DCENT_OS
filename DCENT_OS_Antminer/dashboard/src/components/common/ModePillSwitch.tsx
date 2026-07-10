import React from 'react';
import { useMinerStore } from '../../store/miner';
import { MODE_DESCRIPTIONS } from '../../utils/constants';
import { useModeNavigation } from '../../hooks/useModeNavigation';
import type { OperatingMode } from '../../api/types';

const MODES: { id: OperatingMode; label: string }[] = [
  { id: 'heater', label: 'Heat' },
  { id: 'standard', label: 'Mining' },
  { id: 'hacker', label: 'Hacker' },
];

export function ModePillSwitch() {
  const currentMode = useMinerStore(s => s.mode);
  const { switchMode, switchingMode } = useModeNavigation();

  return (
    <div className="mode-pill-switch" role="group" aria-label="Dashboard mode switcher">
      {MODES.map(mode => {
        const isActive = currentMode === mode.id;
        const info = MODE_DESCRIPTIONS[mode.id];
        const isBusy = switchingMode === mode.id;

        return (
          <button
            key={mode.id}
            className={`mode-pill-btn ${isActive ? 'active' : ''}`}
            onClick={() => { void switchMode(mode.id); }}
            aria-pressed={isActive}
            disabled={switchingMode !== null}
            title={`${info.title}: ${info.subtitle}`}
          >
            {isBusy ? '...' : mode.label}
          </button>
        );
      })}
    </div>
  );
}
