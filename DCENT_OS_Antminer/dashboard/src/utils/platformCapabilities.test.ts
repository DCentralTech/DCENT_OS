import { describe, it, expect } from 'vitest';
import {
  platformCapabilities,
  platformGateFromDeviceCapability,
  tierFromPlatformKey,
  isHardwareToolVisible,
  HARDWARE_TOOL_CAPABILITY,
  UNKNOWN_PLATFORM_CAPS,
  type PlatformTier,
  type PlatformCapabilities,
} from './platformCapabilities';
import type { DeviceCapabilityDescriptor } from '../api/generated/capability';

// TEST-DASH-3 (LANE A): the platform capability matrix is the fail-closed
// source of truth for per-platform Advanced-tool gating. These tests pin
// (1) the daemon platform_key → canonical tier coercion, (2) the fail-closed
// unknown-platform defaults, (3) each tier's exact capability set, and
// (4) the hardware-tool visibility gates the Advanced shell consumes.

function antminerDescriptor(
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
      writeDeniedAddrs: [0x50, 0x51],
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
    power: { ...base.power, ...overrides.power },
  };
}

describe('tierFromPlatformKey', () => {
  it('maps every canonical daemon platform_key to its tier', () => {
    expect(tierFromPlatformKey('am1-zynq')).toBe('am1-zynq');
    expect(tierFromPlatformKey('am2-zynq')).toBe('am2-zynq');
    expect(tierFromPlatformKey('am3-aml')).toBe('am3-aml');
    expect(tierFromPlatformKey('am3-bb')).toBe('am3-bb');
    expect(tierFromPlatformKey('unknown')).toBe('unknown');
  });

  it('collapses board-suffixed and case/whitespace variants to the tier', () => {
    // The daemon may emit a longer board-specific key; tolerate it.
    expect(tierFromPlatformKey('am1-zynq-s9')).toBe('am1-zynq');
    expect(tierFromPlatformKey('am2-zynq-s19jpro')).toBe('am2-zynq');
    expect(tierFromPlatformKey('AM2-ZYNQ')).toBe('am2-zynq');
    expect(tierFromPlatformKey('  am3-aml-s19k  ')).toBe('am3-aml');
    expect(tierFromPlatformKey('am3aml')).toBe('am3-aml');
    expect(tierFromPlatformKey('am3bb')).toBe('am3-bb');
    expect(tierFromPlatformKey('am3-bb-s19jpro')).toBe('am3-bb');
  });

  it('fails closed to "unknown" for missing / empty / typo keys', () => {
    // Older daemon that doesn't emit platform_key, or a fresh/never-loaded
    // systemInfo — must NOT guess a platform.
    expect(tierFromPlatformKey(undefined)).toBe('unknown');
    expect(tierFromPlatformKey('')).toBe('unknown');
    expect(tierFromPlatformKey('   ')).toBe('unknown');
    expect(tierFromPlatformKey('amlogic')).toBe('unknown');
    expect(tierFromPlatformKey('zynq')).toBe('unknown');
    expect(tierFromPlatformKey('am4-foo')).toBe('unknown');
    expect(tierFromPlatformKey('xil')).toBe('unknown');
  });
});

describe('platformGateFromDeviceCapability', () => {
  it('uses the shared descriptor as the primary gate input', () => {
    const gate = platformGateFromDeviceCapability(antminerDescriptor(), 'unknown');

    expect(gate.source).toBe('descriptor');
    expect(gate.tier).toBe('am2-zynq');
    expect(gate.caps.fpgaInspector).toBe(true);
    expect(gate.caps.dspicDiagnostics).toBe(true);
    expect(isHardwareToolVisible('i2c', gate.caps)).toBe(true);
  });

  it('fails closed for an unknown descriptor even when legacy platform_key is tempting', () => {
    const gate = platformGateFromDeviceCapability(
      antminerDescriptor({
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
      }),
      'am2-zynq',
    );

    expect(gate.source).toBe('descriptor');
    expect(gate.tier).toBe('unknown');
    for (const toolId of Object.keys(HARDWARE_TOOL_CAPABILITY)) {
      expect(isHardwareToolVisible(toolId, gate.caps), toolId).toBe(false);
    }
  });

  it('keeps this Antminer dashboard closed for non-Antminer descriptors', () => {
    const gate = platformGateFromDeviceCapability(
      antminerDescriptor({
        family: 'esp',
        support: 'experimental',
        identity: {
          ...antminerDescriptor().identity,
          boardTarget: 'bitaxe-gamma',
          platform: 'esp32-s3',
        },
      }),
      'am1-zynq',
    );

    expect(gate.source).toBe('descriptor');
    expect(gate.tier).toBe('unknown');
    expect(isHardwareToolVisible('fpga', gate.caps)).toBe(false);
    expect(isHardwareToolVisible('voltage', gate.caps)).toBe(false);
  });

  it('falls back to platform_key only when the descriptor is absent', () => {
    const gate = platformGateFromDeviceCapability(null, 'am1-zynq');

    expect(gate.source).toBe('fallback');
    expect(gate.tier).toBe('am1-zynq');
    expect(gate.caps.pic16f1704Diagnostics).toBe(true);
  });

  it('uses controller detail to avoid assuming every AM3 unit has dsPIC', () => {
    const gate = platformGateFromDeviceCapability(antminerDescriptor({
      identity: {
        ...antminerDescriptor().identity,
        boardTarget: 'antminer-amlogic',
        boardVersion: 'AML S21',
      },
      board: {
        ...antminerDescriptor().board,
        boardTarget: 'antminer-amlogic',
        controlBoard: 'AML S21',
      },
      controlBoard: {
        soc: 'AML S21',
        controlBoardId: 'AML S21',
        uioModel: null,
      },
      power: {
        ...antminerDescriptor().power,
        voltageControl: 'nopic',
      },
      controllers: [{
        kind: 'tas-no-pic',
        fwVersion: null,
        writeDeniedAddrs: [0x50],
        degradedFwRefuse: false,
      }],
    }));

    expect(gate.tier).toBe('am3-aml');
    expect(gate.caps.dspicDiagnostics).toBe(false);
    expect(gate.caps.tas5782mDacDiagnostics).toBe(true);
    expect(isHardwareToolVisible('voltage', gate.caps)).toBe(true);
  });
});

describe('UNKNOWN_PLATFORM_CAPS', () => {
  // Every flag in this set that is NOT stratumV2Cards is a "hardware" flag and
  // must default to false so an unknown platform exposes no platform-specific
  // bench tool. stratumV2Cards is platform-agnostic and stays true.
  const hardwareFlags: Array<keyof PlatformCapabilities> = (
    Object.keys(UNKNOWN_PLATFORM_CAPS) as Array<keyof PlatformCapabilities>
  ).filter(k => k !== 'stratumV2Cards');

  it('has every hardware flag false (fail closed)', () => {
    for (const flag of hardwareFlags) {
      expect(UNKNOWN_PLATFORM_CAPS[flag], `${String(flag)} must be false`).toBe(false);
    }
  });

  it('keeps the platform-agnostic SV2 cards enabled', () => {
    expect(UNKNOWN_PLATFORM_CAPS.stratumV2Cards).toBe(true);
  });

  it('is what platformCapabilities returns for the unknown tier', () => {
    expect(platformCapabilities('unknown')).toEqual(UNKNOWN_PLATFORM_CAPS);
  });
});

describe('platformCapabilities — per-tier matrix', () => {
  // The expected capability set for each tier, mirroring PLATFORM_MATRIX.md.
  // Pinning the full object catches any accidental flag drift.
  const EXPECTED: Record<Exclude<PlatformTier, 'unknown'>, PlatformCapabilities> = {
    'am1-zynq': {
      ...UNKNOWN_PLATFORM_CAPS,
      fpgaInspector: true,
      pic16f1704Diagnostics: true,
    },
    'am2-zynq': {
      ...UNKNOWN_PLATFORM_CAPS,
      fpgaInspector: true,
      dspicDiagnostics: true,
      hashboardEepromReader: true,
    },
    'am3-aml': {
      ...UNKNOWN_PLATFORM_CAPS,
      serialChainConsole: true,
      dspicDiagnostics: true,
      tas5782mDacDiagnostics: true,
      apwPmbusTelemetry: true,
      amlogicImageInstaller: true,
      hashboardEepromReader: true,
      gdtunerIndicator: true,
      twentyOneStepProfileSelector: true,
    },
    'am3-bb': {
      ...UNKNOWN_PLATFORM_CAPS,
      serialChainConsole: true,
      dspicDiagnostics: true,
      bbSdCardRecovery: true,
      gdtunerIndicator: true,
      twentyOneStepProfileSelector: true,
    },
  };

  it.each(Object.keys(EXPECTED) as Array<Exclude<PlatformTier, 'unknown'>>)(
    'returns the exact capability set for %s',
    (tier) => {
      expect(platformCapabilities(tier)).toEqual(EXPECTED[tier]);
    },
  );

  it('only am1/am2 (Zynq) expose the FPGA inspector', () => {
    expect(platformCapabilities('am1-zynq').fpgaInspector).toBe(true);
    expect(platformCapabilities('am2-zynq').fpgaInspector).toBe(true);
    // Amlogic has NO FPGA — the inspector would shell garbage Zynq addresses.
    expect(platformCapabilities('am3-aml').fpgaInspector).toBe(false);
    expect(platformCapabilities('am3-bb').fpgaInspector).toBe(false);
    expect(platformCapabilities('unknown').fpgaInspector).toBe(false);
  });

  it('only am3-bb exposes the BB SD-card recovery card', () => {
    expect(platformCapabilities('am3-bb').bbSdCardRecovery).toBe(true);
    expect(platformCapabilities('am1-zynq').bbSdCardRecovery).toBe(false);
    expect(platformCapabilities('am2-zynq').bbSdCardRecovery).toBe(false);
    expect(platformCapabilities('am3-aml').bbSdCardRecovery).toBe(false);
    expect(platformCapabilities('unknown').bbSdCardRecovery).toBe(false);
  });
});

describe('isHardwareToolVisible — Advanced-tool gating', () => {
  it('hides FPGA Regs on Amlogic but shows it on Zynq', () => {
    expect(isHardwareToolVisible('fpga', platformCapabilities('am2-zynq'))).toBe(true);
    expect(isHardwareToolVisible('fpga', platformCapabilities('am1-zynq'))).toBe(true);
    expect(isHardwareToolVisible('fpga', platformCapabilities('am3-aml'))).toBe(false);
  });

  it('gates i2c / voltage / psu on ANY voltage controller (dsPIC or PIC16F1704)', () => {
    // am1-s9 uses a PIC16F1704 voltage controller — the I2C Scanner decodes the
    // S9 PICs at 0x55-0x57, so these MUST stay visible on the S9 beta-gate unit
    // (regression guard: do NOT gate on dspicDiagnostics alone).
    const am1 = platformCapabilities('am1-zynq');
    expect(isHardwareToolVisible('i2c', am1)).toBe(true);
    expect(isHardwareToolVisible('voltage', am1)).toBe(true);
    expect(isHardwareToolVisible('psu', am1)).toBe(true);

    const am2 = platformCapabilities('am2-zynq');
    expect(isHardwareToolVisible('i2c', am2)).toBe(true);
    expect(isHardwareToolVisible('voltage', am2)).toBe(true);
    expect(isHardwareToolVisible('psu', am2)).toBe(true);

    // Unknown platform has neither voltage-controller flag → fail closed.
    expect(isHardwareToolVisible('i2c', UNKNOWN_PLATFORM_CAPS)).toBe(false);
    expect(isHardwareToolVisible('voltage', UNKNOWN_PLATFORM_CAPS)).toBe(false);
    expect(isHardwareToolVisible('psu', UNKNOWN_PLATFORM_CAPS)).toBe(false);
  });

  it('shows the UART FIFO tool when either FPGA or serial-chain is present', () => {
    // Zynq: via fpgaInspector. Amlogic/BB: via serialChainConsole.
    expect(isHardwareToolVisible('uart', platformCapabilities('am1-zynq'))).toBe(true);
    expect(isHardwareToolVisible('uart', platformCapabilities('am2-zynq'))).toBe(true);
    expect(isHardwareToolVisible('uart', platformCapabilities('am3-aml'))).toBe(true);
    expect(isHardwareToolVisible('uart', platformCapabilities('am3-bb'))).toBe(true);
    // Unknown platform has neither → hidden.
    expect(isHardwareToolVisible('uart', UNKNOWN_PLATFORM_CAPS)).toBe(false);
  });

  it('hides every gated hardware tool on an unknown platform (fail closed)', () => {
    for (const toolId of Object.keys(HARDWARE_TOOL_CAPABILITY)) {
      expect(
        isHardwareToolVisible(toolId, UNKNOWN_PLATFORM_CAPS),
        `${toolId} must be hidden on unknown platform`,
      ).toBe(false);
    }
  });

  it('leaves platform-agnostic tools visible on every platform', () => {
    const agnostic = ['dashboard', 'console', 'chipmap', 'sv2', 'diagnostics', 'api', 'journal'];
    const tiers: PlatformTier[] = ['am1-zynq', 'am2-zynq', 'am3-aml', 'am3-bb', 'unknown'];
    for (const tier of tiers) {
      const caps = platformCapabilities(tier);
      for (const toolId of agnostic) {
        expect(
          isHardwareToolVisible(toolId, caps),
          `${toolId} must stay visible on ${tier}`,
        ).toBe(true);
      }
    }
  });
});
