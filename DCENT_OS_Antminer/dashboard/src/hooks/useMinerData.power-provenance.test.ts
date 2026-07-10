import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

const hookSource = readFileSync('src/hooks/useMinerData.ts', 'utf8');
const typeSource = readFileSync('src/api/types.ts', 'utf8');
const statusResponseSource = typeSource.slice(
  typeSource.indexOf('export interface StatusResponse'),
  typeSource.indexOf('export interface ConfigResponse'),
);

describe('WebSocket power provenance handling', () => {
  it('preserves WebSocket power provenance in stats and history', () => {
    expect(typeSource).toContain('power_source?:');
    expect(typeSource).toContain('power_source_detail?:');
    expect(typeSource).toContain('live_power_available?: boolean');
    expect(typeSource).toContain('power_modeled?: boolean');
    expect(typeSource).toContain('power_calibrated?: boolean');
    expect(hookSource).toContain('source: msg.power_source ?? prevPowerSource');
    expect(hookSource).toContain('source_detail: msg.power_source_detail ?? prevPowerSourceDetail');
    expect(hookSource).toContain('live_power_available: msg.live_power_available ?? prevLivePowerAvailable');
    expect(hookSource).toContain('modeled: msg.power_modeled ?? prevPowerModeled');
    expect(hookSource).toContain('getLiveWallWatts({');
    expect(hookSource).toContain('const power = wsPower > 0 ? wsPower : getLiveWallWatts(statsState?.power)');
    expect(hookSource).not.toContain('const power = wsPower > 0 ? wsPower : getWallWatts(statsState?.power)');
  });
});

describe('REST status power provenance contract', () => {
  it('models /api/status power with live/fallback provenance fields', () => {
    expect(statusResponseSource).toContain('source_detail?:');
    expect(statusResponseSource).toContain("'static_model_fallback'");
    expect(statusResponseSource).toContain("'static_power_fallback_from_miner_state'");
    expect(statusResponseSource).toContain('live_power_available?: boolean');
    expect(statusResponseSource).toContain('modeled?: boolean');
    expect(statusResponseSource).toContain('note?: string');
  });
});
