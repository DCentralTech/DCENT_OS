import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

const source = readFileSync('src/components/features/ProfileSharing.tsx', 'utf8');

describe('ProfileSharing power honesty', () => {
  it('exports J/TH only when current power telemetry has live provenance', () => {
    expect(source).toContain("import { getLivePowerEfficiencyJth } from '../../utils/power'");
    expect(source).toContain('const liveEfficiencyJth = getLivePowerEfficiencyJth(stats?.power)');
    expect(source).toContain('efficiencyJth: liveEfficiencyJth > 0 ? liveEfficiencyJth : null');
    expect(source).toContain('live wall-power-backed');
    expect(source).not.toContain('efficiencyJth: (stats?.power?.efficiency_jth) ?? null');
  });
});
