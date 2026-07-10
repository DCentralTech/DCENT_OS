// @vitest-environment jsdom

import { describe, expect, it } from 'vitest';
import {
  FX_SETTINGS_KEY,
  applyVitalityAttribute,
  initPageVisibilityAttribute,
  initVitalityAttribute,
  readFxSettings,
  writeFxSettings,
} from './fxSettings';

class MemoryStorage {
  values = new Map<string, string>();
  getItem(key: string) {
    return this.values.get(key) ?? null;
  }
  setItem(key: string, value: string) {
    this.values.set(key, value);
  }
}

describe('fxSettings', () => {
  it('normalizes persisted settings with safe defaults', () => {
    const storage = new MemoryStorage();
    storage.setItem(FX_SETTINGS_KEY, JSON.stringify({ enabled: false, vitality: 'calm' }));

    expect(readFxSettings(storage)).toEqual({
      enabled: false,
      vitality: 'calm',
      titleTicker: true,
    });
  });

  it('writes partial settings over the current value', () => {
    const storage = new MemoryStorage();
    expect(writeFxSettings({ vitality: 'calm' }, storage)).toEqual({
      enabled: true,
      vitality: 'calm',
      titleTicker: true,
    });
  });

  it('stamps the calm vitality preference on the root element', () => {
    const storage = new MemoryStorage();
    storage.setItem(FX_SETTINGS_KEY, JSON.stringify({ vitality: 'calm' }));
    const root = document.createElement('html');
    const fakeDoc = { documentElement: root } as unknown as Document;

    const cleanup = initVitalityAttribute(fakeDoc, storage);
    expect(root.getAttribute('data-vitality')).toBe('calm');

    applyVitalityAttribute({ enabled: true, vitality: 'full', titleTicker: true }, fakeDoc);
    expect(root.hasAttribute('data-vitality')).toBe(false);

    cleanup();
    expect(root.hasAttribute('data-vitality')).toBe(false);
  });

  it('stamps page-hidden state on the root element', () => {
    const listeners = new Map<string, () => void>();
    const root = document.createElement('html');
    const fakeDoc = {
      documentElement: root,
      hidden: false,
      addEventListener: (event: string, fn: () => void) => listeners.set(event, fn),
      removeEventListener: (event: string) => listeners.delete(event),
    } as unknown as Document;

    const cleanup = initPageVisibilityAttribute(fakeDoc);
    expect(root.hasAttribute('data-page-hidden')).toBe(false);

    Object.defineProperty(fakeDoc, 'hidden', { value: true, configurable: true });
    listeners.get('visibilitychange')?.();
    expect(root.getAttribute('data-page-hidden')).toBe('true');

    cleanup();
    expect(root.hasAttribute('data-page-hidden')).toBe(false);
  });
});
