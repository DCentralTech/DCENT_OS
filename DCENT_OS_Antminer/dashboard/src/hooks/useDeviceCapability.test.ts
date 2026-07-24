// @vitest-environment jsdom

import { cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DeviceCapabilityDescriptor } from '../api/generated/capability';

const h = vi.hoisted(() => ({
  api: {
    getDeviceCapability: vi.fn(),
  },
}));

vi.mock('../api/client', () => ({ api: h.api }));

import { useDeviceCapability } from './useDeviceCapability';

function descriptor(
  overrides: Partial<DeviceCapabilityDescriptor> = {},
): DeviceCapabilityDescriptor {
  const base: DeviceCapabilityDescriptor = {
    schemaVersion: 3,
    family: 'antminer',
    identity: {
      confidence: 'exact',
      sources: ['test'],
      note: null,
      deviceModel: 'Antminer S19j Pro',
      boardTarget: 'antminer-zynq-am2',
      boardVersion: 'Zynq am2-s19jpro',
      platform: 'dcentos-antminer',
    },
    support: 'beta',
    board: {
      boardTarget: 'antminer-zynq-am2',
      family: 'antminer',
      controlBoard: 'Zynq am2-s19jpro',
      fixtureRefs: [],
    },
    controlBoard: {
      soc: 'Zynq am2-s19jpro',
      controlBoardId: 'Zynq am2-s19jpro',
      uioModel: null,
    },
    asic: {
      chipModel: 'BM1362',
      asicFamily: 'bitmain-bm13xx',
      chipId: 0x1362,
      baud: 1000000,
      coresPerChip: 112,
      nonceAttributionCores: 894,
    },
    topology: {
      chainCount: 3,
      chipsPerChain: 126,
      fanCount: 4,
      tempSensors: ['board_sensor'],
      hashboards: [],
    },
    fanTopology: {
      controlMode: 'pwm-and-tach',
      fanCount: 4,
      tachChannels: [0, 1, 2, 3],
      pwmChannels: [0, 1, 2, 3],
      perFan: [],
    },
    tempSensors: [],
    thermal: {
      runtimeCaps: ['monitoring'],
      failClosedOnSensorLoss: true,
    },
    power: {
      runtimeCaps: ['monitoring'],
      voltageControl: 'dspic33ep',
      psuProtocol: 'APW',
      psuMode: 'pmbus-monitor',
      psuModel: 'APW',
      writesEnabled: false,
    },
    controllers: [{
      kind: 'dspic33ep',
      fwVersion: null,
      writeDeniedAddrs: [0x50],
      degradedFwRefuse: true,
    }],
    operatingEnvelopes: {
      frequency: null,
      voltage: null,
      fan: { minPwm: 0, maxPwm: 30 },
    },
    references: {
      fixtureRefs: [],
      simProfileRef: null,
      benchChecklistRef: 'bench/antminer-zynq-am2',
    },
    runtimeCaps: ['detect', 'inventory', 'monitoring'],
    install: {
      plannerOutcome: 'runtime-only',
      proofScope: 'exact_target_lab_only',
      requiredCapabilities: [],
      missingCapabilities: [],
      recoveryRouteId: 'antminer-test',
      note: null,
    },
    safeDefaults: {
      miningEnabled: false,
      fanPwmCap: 30,
      frequencyMhz: 500,
      voltageMv: 12000,
    },
    failSafe: {
      readOnly: true,
      miningStartAllowed: false,
      mutatingRoutesAllowed: false,
      reason: 'test',
    },
  };

  return {
    ...base,
    ...overrides,
    identity: { ...base.identity, ...overrides.identity },
    board: { ...base.board, ...overrides.board },
    controlBoard: { ...base.controlBoard, ...overrides.controlBoard },
  };
}

beforeEach(() => {
  h.api.getDeviceCapability.mockReset();
});

afterEach(() => {
  cleanup();
});

describe('useDeviceCapability', () => {
  it('loads the shared descriptor and exposes descriptor-derived platform caps', async () => {
    h.api.getDeviceCapability.mockResolvedValue(descriptor());

    const { result } = renderHook(() => useDeviceCapability('unknown'));

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.error).toBeNull();
    expect(result.current.source).toBe('descriptor');
    expect(result.current.tier).toBe('am2-zynq');
    expect(result.current.caps.fpgaInspector).toBe(true);
  });

  it('uses legacy platform_key fallback only when the descriptor endpoint fails', async () => {
    h.api.getDeviceCapability.mockRejectedValue(new Error('404'));

    const { result } = renderHook(() => useDeviceCapability('am1-zynq'));

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.error).toBe('404');
    expect(result.current.source).toBe('fallback');
    expect(result.current.tier).toBe('am1-zynq');
    expect(result.current.caps.pic16f1704Diagnostics).toBe(true);
  });

  it('does not let an explicit unknown descriptor fall through to fallback gates', async () => {
    h.api.getDeviceCapability.mockResolvedValue(descriptor({
      family: 'unknown',
      support: 'unknown',
      identity: {
        confidence: 'unknown',
        sources: [],
        note: null,
        deviceModel: null,
        boardTarget: null,
        boardVersion: null,
        platform: null,
      },
    }));

    const { result } = renderHook(() => useDeviceCapability('am2-zynq'));

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.source).toBe('descriptor');
    expect(result.current.tier).toBe('unknown');
    expect(result.current.caps.fpgaInspector).toBe(false);
    expect(result.current.caps.pic16f1704Diagnostics).toBe(false);
  });
});
