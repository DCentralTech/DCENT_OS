// Lightweight hash-based router — syncs location.hash with Zustand store's currentPage

import { useEffect } from 'react';
import type { OperatingMode } from '../api/types';

const MODE_DEFAULT_PAGE: Record<OperatingMode, string> = {
  heater: 'heater-home',
  standard: 'dashboard',
  hacker: 'dashboard',
};

export const STANDARD_PRIMARY_PAGES = new Set([
  'dashboard',
  'pools',
  'earnings',
  'temperature',
  'tuning',
  'logs',
  'evidence',
  'settings',
  'energy',
  'integrations',
  'fleet',
  'offgrid',
  'profiles',
  'system',
  'autotuner',
]);

export const STANDARD_SETTINGS_SUBPAGES = [
  { id: 'settings/general', label: 'Settings / General', keywords: ['general', 'identity', 'donation', 'profit'] },
  { id: 'settings/security', label: 'Settings / Security', keywords: ['security', 'password', 'circuit', 'safety'] },
  { id: 'settings/network', label: 'Settings / Network', keywords: ['network', 'hostname', 'ip', 'alerts'] },
  { id: 'settings/backup', label: 'Settings / Backup', keywords: ['backup', 'restore', 'firmware', 'upgrade'] },
  { id: 'settings/appearance', label: 'Settings / Appearance', keywords: ['appearance', 'theme', 'accent', 'led'] },
] as const;

export const HACKER_PRIMARY_PAGES = new Set([
  'dashboard',
  'console',
  'chipmap',
  'sv2',
  'beatlab',
  'fpga',
  'i2c',
  'uart',
  'asic',
  'voltage',
  'psu',
  'pipeline',
  'timeline',
  'replay',
  'fingerprint',
  'blocker',
  'flight',
  'journal',
  'macros',
  'patchbay',
  'session',
  'audit',
  'debug',
  'api',
  'diagnostics',
]);

export const HEATER_PAGES = new Set(['heater-home', 'heater-history', 'heater-settings']);

/** Parse hash like /#/pools or /#/advanced/fpga into a page id */
export function parseHash(): string {
  const hash = location.hash.replace(/^#\/?/, '');
  return hash || 'dashboard';
}

export function getPrimaryPage(page: string): string {
  return page.split('/')[0] || 'dashboard';
}

export function getSubPage(page: string): string | null {
  const [, ...rest] = page.split('/');
  return rest.length > 0 ? rest.join('/') : null;
}

export function getDefaultPageForMode(mode: OperatingMode): string {
  return MODE_DEFAULT_PAGE[mode] ?? MODE_DEFAULT_PAGE.standard;
}

export function normalizePageForMode(mode: OperatingMode, page: string): string {
  const primary = getPrimaryPage(page);
  if (mode === 'heater') {
    return HEATER_PAGES.has(page) ? page : MODE_DEFAULT_PAGE.heater;
  }
  if (mode === 'hacker') {
    return HACKER_PRIMARY_PAGES.has(primary) ? page : MODE_DEFAULT_PAGE.hacker;
  }
  return STANDARD_PRIMARY_PAGES.has(primary) ? page : MODE_DEFAULT_PAGE.standard;
}

/** Set hash from page id (pushState so back button works) */
export function setHash(page: string): void {
  const newHash = '#/' + page;
  if (location.hash !== newHash) {
    history.pushState(null, '', newHash);
  }
}

/** Hook: listen for hashchange events and sync to store */
export function useHashRoute(setCurrentPage: (page: string) => void): void {
  useEffect(() => {
    const handler = () => {
      const page = parseHash();
      setCurrentPage(page);
    };
    window.addEventListener('hashchange', handler);
    // Also handle popstate for back/forward navigation
    window.addEventListener('popstate', handler);
    // Initialize from hash on mount
    const initial = parseHash();
    if (initial !== 'dashboard') {
      setCurrentPage(initial);
    }
    return () => {
      window.removeEventListener('hashchange', handler);
      window.removeEventListener('popstate', handler);
    };
  }, [setCurrentPage]);
}
