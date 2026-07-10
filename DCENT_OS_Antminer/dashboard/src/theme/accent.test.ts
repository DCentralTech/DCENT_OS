import { afterEach, describe, expect, it, vi } from 'vitest';

describe('accent theme module', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.resetModules();
  });

  it('can be imported without DOM globals', async () => {
    vi.stubGlobal('document', undefined);
    vi.stubGlobal('window', undefined);
    vi.resetModules();

    const accent = await import('./accent');

    expect(accent.getAccent()).toBe('#FAA500');
    expect(() => accent.applyAccent('#00FF41')).not.toThrow();
  });
});
