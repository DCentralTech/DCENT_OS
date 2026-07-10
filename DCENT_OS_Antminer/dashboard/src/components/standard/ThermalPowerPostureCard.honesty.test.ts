import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

const source = readFileSync('src/components/standard/ThermalPowerPostureCard.tsx', 'utf8');
const apiTypes = readFileSync('src/api/types.ts', 'utf8');

describe('ThermalPowerPostureCard power honesty', () => {
  it('requires live thermal-posture power provenance before rendering wall watts', () => {
    expect(apiTypes).toContain('live_power_available: boolean');
    expect(apiTypes).toContain('source_detail: string');
    expect(apiTypes).toContain('modeled: boolean');
    expect(apiTypes).toContain('note: string');

    expect(source).toContain('const wallWatts = posture?.power.live_power_available === true');
    expect(source).toContain('const powerNote = posture?.power.note ?? posture?.power.reason');
    expect(source).toContain("posture?.power.calibrated && wallWatts ? 'Wall-meter calibrated estimate'");
    expect(source).toContain("power detail: {posture?.power.source_detail || 'unavailable'}");
    expect(source).not.toContain('const wallWatts = posture?.power.wall_watts ?? null');
  });
});
