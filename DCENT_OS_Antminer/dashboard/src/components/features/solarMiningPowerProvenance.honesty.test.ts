import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

const componentSource = readFileSync('src/components/features/SolarConfig.tsx', 'utf8');
const featureTypesSource = readFileSync('src/api/feature-types.ts', 'utf8');

describe('solar miner-load power provenance', () => {
  it('does not present missing live miner power as an unqualified zero-watt load', () => {
    expect(featureTypesSource).toContain('miningWattsSource?: string');
    expect(featureTypesSource).toContain('miningWattsLive?: boolean');
    expect(featureTypesSource).toContain('miningWattsModeled?: boolean');
    expect(featureTypesSource).toContain('miningWattsNote?: string');

    expect(componentSource).toContain('formatMiningPowerSource');
    expect(componentSource).toContain('Miner load source: unavailable');
    expect(componentSource).toContain('status.miningWattsLive === false && status.miningWatts === 0');
    expect(componentSource).toContain('Unavailable');
    expect(componentSource).toContain('status.miningWattsNote');
  });
});
