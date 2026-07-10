// Shared chain/chip context for advanced mode components
// Uses React context (NOT Zustand) to avoid conflicts with other sprint agents modifying the store

import React, { createContext, useContext, useState, useCallback } from 'react';

export interface ActiveHardwareState {
  activeChain: number;
  activeChip: number | null;  // null = all chips / no specific chip selected
  setActiveChain: (chain: number) => void;
  setActiveChip: (chip: number | null) => void;
}

const ActiveHardwareContext = createContext<ActiveHardwareState>({
  activeChain: 6,
  activeChip: null,
  setActiveChain: () => {},
  setActiveChip: () => {},
});

export function ActiveHardwareProvider({ children }: { children: React.ReactNode }) {
  const [activeChain, setActiveChainRaw] = useState(6);
  const [activeChip, setActiveChipRaw] = useState<number | null>(null);

  const setActiveChain = useCallback((chain: number) => {
    setActiveChainRaw(chain);
  }, []);

  const setActiveChip = useCallback((chip: number | null) => {
    setActiveChipRaw(chip);
  }, []);

  return React.createElement(
    ActiveHardwareContext.Provider,
    {
      value: { activeChain, activeChip, setActiveChain, setActiveChip },
    },
    children
  );
}

export function useActiveHardware(): ActiveHardwareState {
  return useContext(ActiveHardwareContext);
}
