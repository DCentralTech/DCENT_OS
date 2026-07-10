import { beforeAll, describe, expect, it, vi } from 'vitest';
import { HACKER_PRIMARY_PAGES, normalizePageForMode } from './router';

describe('hacker router allow-list', () => {
  let advancedToolIds: string[] = [];

  beforeAll(async () => {
    const storage = new Map<string, string>();
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, value),
      removeItem: (key: string) => storage.delete(key),
      clear: () => storage.clear(),
    });
    const advancedDashboard = await import('../components/advanced/AdvancedDashboard');
    advancedToolIds = advancedDashboard.ADVANCED_TOOL_IDS;
  });

  it('keeps every AdvancedDashboard catalog tool deep-linkable', () => {
    for (const toolId of advancedToolIds) {
      expect(
        HACKER_PRIMARY_PAGES.has(toolId),
        `${toolId} is in the AdvancedDashboard tool catalog but is not routable in Hacker mode`,
      ).toBe(true);
      expect(normalizePageForMode('hacker', toolId)).toBe(toolId);
    }
  });

  it('preserves the audit page deep link on refresh', () => {
    expect(normalizePageForMode('hacker', 'audit')).toBe('audit');
  });
});
