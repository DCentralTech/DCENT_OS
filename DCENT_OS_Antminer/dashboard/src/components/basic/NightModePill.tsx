import React from 'react';
import { useMinerStore } from '../../store/miner';
import { glossaryText } from '../../utils/glossary';

export function NightModePill() {
  const nightMode = useMinerStore(s => s.nightMode);
  const setCurrentPage = useMinerStore(s => s.setCurrentPage);

  if (!nightMode?.active) return null;

  return (
    <button
      type="button"
      className="night-mode-pill"
      onClick={() => setCurrentPage('heater-settings')}
      data-tooltip={glossaryText('night_mode_behaviour')}
      aria-label="Night mode is active. Open heater settings"
    >
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
      </svg>
      <span>Night Mode</span>
    </button>
  );
}
