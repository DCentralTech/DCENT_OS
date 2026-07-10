import { beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
  state: {
    status: {
      hashrate_ghs: 1000,
      accepted: 10,
      rejected: 1,
      chains: [{ temp_c: 55 }],
      pool: { status: 'mining' },
    },
    stats: null as null | { power?: Record<string, unknown> },
  },
}));

vi.mock('../api/client', () => ({ api: {} }));
vi.mock('../store/miner', () => ({
  useMinerStore: {
    getState: () => h.state,
  },
}));

import { COMPANION_SYSTEM_PROMPT, findTool } from './tools';

describe('companion status power honesty', () => {
  beforeEach(() => {
    h.state.stats = null;
  });

  it('does not report static fallback watts as current live power', async () => {
    h.state.stats = {
      power: {
        watts: 1100,
        wall_watts: 1234,
        source: 'static_model_fallback',
        source_detail: 'static_power_fallback_from_miner_state',
        live_power_available: false,
        modeled: true,
      },
    };

    const tool = findTool('get_miner_status');
    const payload = JSON.parse(await tool!.run({}));

    expect(payload.power_w).toBeNull();
    expect(payload.power_live).toBe(false);
    expect(payload.power_source).toBe('static_model_fallback');
    expect(payload.power_source_detail).toBe('static_power_fallback_from_miner_state');
    expect(payload.power_note).toBe('Modeled fallback estimate');
  });

  it('reports measured live wall power with provenance', async () => {
    h.state.stats = {
      power: {
        watts: 1000,
        wall_watts: 1105,
        source: 'pmbus',
        source_detail: 'pmbus_measured',
        live_power_available: true,
        modeled: false,
      },
    };

    const tool = findTool('get_miner_status');
    const payload = JSON.parse(await tool!.run({}));

    expect(payload.power_w).toBe(1105);
    expect(payload.power_live).toBe(true);
    expect(payload.power_source).toBe('pmbus');
    expect(payload.power_source_detail).toBe('pmbus_measured');
    expect(payload.power_note).toBe('PMBus measured power');
  });

  it('instructs the companion not to infer current watts when live power is unavailable', () => {
    expect(COMPANION_SYSTEM_PROMPT).toContain('power_w:null');
    expect(COMPANION_SYSTEM_PROMPT).toContain('do not estimate current watts');
  });
});
