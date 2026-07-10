// Mode switching logic — UI-only, no reboot required

import { useMinerStore } from '../store/miner';
import { setHash } from '../utils/router';
import type { OperatingMode } from '../api/types';

export function useMode() {
  const mode = useMinerStore(s => s.mode);
  const setMode = useMinerStore(s => s.setMode);
  const navState = useMinerStore(s => s.navState);

  const switchMode = (newMode: OperatingMode) => {
    // Mode is a client-side UI preference — just switch the view instantly.
    // The daemon's operational mode is separate and configured via Settings.
    setMode(newMode);
    // Sync restored page to URL hash
    setHash(navState[newMode]);
  };

  return { mode, switchMode };
}
