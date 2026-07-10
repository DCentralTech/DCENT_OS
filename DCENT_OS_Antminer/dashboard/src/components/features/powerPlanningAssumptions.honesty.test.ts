import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import { describe, expect, it } from 'vitest';

const FEATURE_DIR = join(process.cwd(), 'src/components/features');

function readFeature(name: string): string {
  return readFileSync(join(FEATURE_DIR, name), 'utf8');
}

describe('planning power assumptions', () => {
  it('does not present the 1100 W planning default as current measured load', () => {
    const circuit = readFeature('CircuitCalculator.tsx');
    const tou = readFeature('TouScheduler.tsx');

    expect(circuit).toContain('getLiveWallWatts');
    expect(tou).toContain('getLiveWallWatts');
    expect(circuit).toContain('Planning load');
    expect(tou).toContain('Planning Daily Cost');
    expect(circuit).toContain('it is not a measured current draw');
    expect(tou).toContain('it is not a measured current draw');
    expect(circuit).not.toContain('Current usage estimated using 1100 W');
    expect(tou).not.toContain('Cost estimated using 1100 W');
  });
});
