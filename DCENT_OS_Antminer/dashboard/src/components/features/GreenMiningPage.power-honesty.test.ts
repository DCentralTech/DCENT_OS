import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

const source = readFileSync('src/components/features/GreenMiningPage.tsx', 'utf8');

describe('Green Mining live power honesty', () => {
  it('does not derive stored-energy metrics from modeled fallback display watts', () => {
    expect(source).toContain('getLiveWallWatts');
    expect(source).toContain('hasLiveWallPower');
    expect(source).toContain('energyStoredTodayKwh != null');
    expect(source).toContain('Energy and efficiency remain unavailable until live wall-power telemetry is reported.');
    expect(source).not.toContain('const watts = getWallWatts');
  });
});
