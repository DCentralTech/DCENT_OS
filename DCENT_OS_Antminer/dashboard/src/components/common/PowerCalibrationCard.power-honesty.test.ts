import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

const card = readFileSync('src/components/common/PowerCalibrationCard.tsx', 'utf8');
const types = readFileSync('src/api/types.ts', 'utf8');

describe('PowerCalibrationCard power honesty', () => {
  it('surfaces source provenance for current calibration watts', () => {
    expect(types).toContain('power_source_detail?: string');
    expect(types).toContain('live_power_available?: boolean');
    expect(types).toContain('power_modeled?: boolean');
    expect(types).toContain('power_note?: string');

    expect(card).toContain('function calibrationPowerLabel');
    expect(card).toContain("calibration.power_source_detail === 'pmbus_measured'");
    expect(card).toContain("calibration.power_source_detail === 'adc_measured'");
    expect(card).toContain('calibration.live_power_available === false');
    expect(card).toContain('Modeled runtime estimate');
    expect(card).toContain('calibration?.power_note');
  });
});
