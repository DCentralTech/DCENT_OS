import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

const pageSource = readFileSync('src/components/autotuner/AutoTunerPage.tsx', 'utf8');
const cardSource = readFileSync('src/components/standard/AutotunerCard.tsx', 'utf8');

describe('Autotuner efficiency honesty', () => {
  it('labels autotuner status efficiency as estimated rather than live wall-power efficiency', () => {
    expect(pageSource).toContain('Est. J/TH <InfoDot term="efficiency_jth"');
    expect(cardSource).toContain('Est. J/TH <InfoDot term="efficiency_jth"');
    expect(cardSource).toContain('Tuner-estimated J/TH');
    expect(pageSource).not.toContain('<div className="ahr-label">J/TH <InfoDot term="efficiency_jth"');
    expect(cardSource).not.toContain('<div className="kpi-label">J/TH <InfoDot term="efficiency_jth"');
  });
});
