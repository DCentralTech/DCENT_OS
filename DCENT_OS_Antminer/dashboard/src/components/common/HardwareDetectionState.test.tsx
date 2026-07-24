// @vitest-environment jsdom

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import type { DeviceCapabilityDescriptor } from '../../api/generated/capability';
import { HardwareDetectionState } from './HardwareDetectionState';

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
      readOnly: false,
      miningStartAllowed: true,
      mutatingRoutesAllowed: true,
      reason: null,
    },
  };

  return {
    ...base,
    ...overrides,
    identity: { ...base.identity, ...overrides.identity },
    failSafe: { ...base.failSafe, ...overrides.failSafe },
  };
}

afterEach(() => {
  cleanup();
});

describe('HardwareDetectionState', () => {
  it('stays quiet for a known descriptor with mutating routes allowed', () => {
    render(<HardwareDetectionState descriptor={descriptor()} />);
    expect(screen.queryByTestId('hardware-detection-state')).toBeNull();
  });

  it('surfaces loading and legacy fallback as status copy', () => {
    const { rerender } = render(<HardwareDetectionState descriptor={null} loading />);
    expect(screen.getByTestId('hardware-detection-state').textContent).toContain('Loading');
    expect(screen.getByRole('status')).toBeTruthy();

    rerender(<HardwareDetectionState descriptor={null} error="404" />);
    expect(screen.getByTestId('hardware-detection-state').textContent).toContain('legacy platform detection');
    expect(screen.getByRole('status')).toBeTruthy();
  });

  it('renders unknown hardware as an alert with the exact support tier', () => {
    render(<HardwareDetectionState descriptor={descriptor({
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
      failSafe: {
        readOnly: true,
        miningStartAllowed: false,
        mutatingRoutesAllowed: false,
        reason: 'unknown descriptor',
      },
    })} />);

    expect(screen.getByRole('alert').textContent).toContain('Unknown hardware');
    expect(screen.getByTestId('hardware-detection-state-tier').textContent).toBe('Unknown');
  });
});
