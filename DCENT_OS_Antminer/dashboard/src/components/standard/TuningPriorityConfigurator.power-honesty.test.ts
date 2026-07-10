import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

const source = readFileSync('src/components/standard/TuningPriorityConfigurator.tsx', 'utf8');

describe('TuningPriorityConfigurator power baseline honesty', () => {
  it('does not arm the tuning baseline from display or static fallback watts', () => {
    expect(source).toContain("import { getLiveDisplayWallWatts } from '../../utils/power'");
    expect(source).toContain('const basePower = getLiveDisplayWallWatts(heater, stats?.power)');
    expect(source).toContain('const hasBaseline = baseHashrate > 0 && basePower > 0');
    expect(source).toContain('No live hashrate/wall-power baseline yet');
    expect(source).not.toContain('const basePower = getDisplayPowerWatts(heater, stats?.power)');
  });
});
