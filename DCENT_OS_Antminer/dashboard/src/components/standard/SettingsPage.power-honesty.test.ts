import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

const source = readFileSync('src/components/standard/settings/GeneralTab.tsx', 'utf8');

describe('Settings profitability power honesty', () => {
  it('does not label standby fallback watts as live power', () => {
    expect(source).toContain('getLiveWallWatts');
    expect(source).toContain('Based on live wall power');
    expect(source).toContain('Power unavailable; cost uses');
    expect(source).toContain('standby assumption');
    expect(source).not.toContain('Based on live power: {watts}W');
  });
});
