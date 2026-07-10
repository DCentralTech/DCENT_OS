import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

const source = readFileSync('src/components/common/CompanionCard.tsx', 'utf8');

describe('CompanionCard power honesty', () => {
  it('unlocks wall-power achievements only from live wall-power telemetry', () => {
    expect(source).toContain("import { getLivePowerEfficiencyJth, getLiveWallWatts } from '../../utils/power'");
    expect(source).toContain('const wallWatts = getLiveWallWatts(power)');
    expect(source).toContain('const efficiencyJth = getLivePowerEfficiencyJth(power)');
    expect(source).toContain('Reach 500 W live at the wall.');
    expect(source).toContain('Run under 85 J/TH with live wall power.');
    expect(source).not.toContain('getWallWatts(power)');
    expect(source).not.toContain("typeof power.efficiency_jth === 'number'");
  });
});
