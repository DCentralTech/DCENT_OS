// Keyboard shortcuts for advanced mode
// Ctrl+1-9 for tool navigation, Ctrl+L for console focus, Escape for modals, ? for help

import { useEffect, useCallback, useState } from 'react';
import { useMinerStore } from '../store/miner';

export const ADVANCED_SHORTCUT_KEYS: Record<string, string> = {
  console: 'Alt+1',
  chipmap: 'Alt+2',
  sv2: 'Alt+3',
  fpga: 'Alt+4',
  i2c: 'Alt+5',
  asic: 'Alt+6',
  voltage: 'Alt+7',
  api: 'Alt+8',
  diagnostics: 'Alt+9',
};

const ADVANCED_SHORTCUT_ROUTES = [
  { page: 'console', description: 'Console' },
  { page: 'chipmap', description: 'Chip Map' },
  { page: 'sv2', description: 'Protocol Inspector' },
  { page: 'fpga', description: 'FPGA Registers' },
  { page: 'i2c', description: 'I2C Bus' },
  { page: 'asic', description: 'ASIC Commander' },
  { page: 'voltage', description: 'Voltage And PID' },
  { page: 'api', description: 'API Explorer' },
  { page: 'diagnostics', description: 'Diagnostics' },
] as const;

export interface ShortcutEntry {
  keys: string;
  description: string;
}

export const SHORTCUT_REFERENCE: ShortcutEntry[] = [
  { keys: 'Ctrl+K', description: 'Command palette' },
  ...ADVANCED_SHORTCUT_ROUTES.map(route => ({
    keys: ADVANCED_SHORTCUT_KEYS[route.page],
    description: route.description,
  })),
  { keys: '`', description: 'Toggle console' },
  { keys: 'Escape', description: 'Close modal / panel' },
  { keys: '?', description: 'Toggle shortcut reference' },
];

interface UseKeyboardShortcutsOptions {
  setCurrentPage: (page: string) => void;
  onConsoleFocus?: () => void;
  onEscape?: () => void;
  onCommandPalette?: () => void;
}

export function useKeyboardShortcuts({
  setCurrentPage,
  onConsoleFocus,
  onEscape,
  onCommandPalette,
}: UseKeyboardShortcutsOptions) {
  const [showHelp, setShowHelp] = useState(false);

  const handleKeyDown = useCallback((e: KeyboardEvent) => {
    // Don't capture when typing in inputs/textareas
    const tag = (e.target as HTMLElement)?.tagName;
    const isInput = tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT';

    // Ctrl+K or Cmd+K: command palette
    if ((e.ctrlKey || e.metaKey) && e.key === 'k') {
      e.preventDefault();
      onCommandPalette?.();
      return;
    }

    // Alt+number: navigate to tool without hijacking browser tab shortcuts.
    if (e.altKey && !e.shiftKey && !e.ctrlKey && !e.metaKey) {
      const num = parseInt(e.key);
      if (num >= 1 && num <= 9) {
        e.preventDefault();
        const route = ADVANCED_SHORTCUT_ROUTES[num - 1];
        setCurrentPage(route.page);
        if (route.page === 'console') {
          onConsoleFocus?.();
        }
        return;
      }
    }

    // Escape: close modals
    if (e.key === 'Escape') {
      setShowHelp(false);
      onEscape?.();
      return;
    }

    // ? key (not in input): toggle help
    if (e.key === '?' && !isInput) {
      e.preventDefault();
      setShowHelp(prev => !prev);
      return;
    }

    // Backtick (not in input, no modifiers): toggle console panel
    if (e.key === '`' && !e.ctrlKey && !e.metaKey && !e.altKey && !isInput) {
      e.preventDefault();
      const current = useMinerStore.getState().currentPage;
      if (current === 'console') {
        const prev = sessionStorage.getItem('pre-console-page') || 'dashboard';
        setCurrentPage(prev);
      } else {
        sessionStorage.setItem('pre-console-page', current);
        setCurrentPage('console');
      }
    }
  }, [setCurrentPage, onConsoleFocus, onEscape, onCommandPalette]);

  useEffect(() => {
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [handleKeyDown]);

  return { showHelp, setShowHelp };
}
