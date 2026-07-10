//  ui-02 — platform capability gating (HAL-free, dashboard-side).
//
// Mirrors the firmware's PLATFORM_MATRIX.md tier definitions
//. The dashboard uses
// these flags to hide / disable cards and panels that don't apply to the
// running platform — e.g. the FPGA register inspector is meaningless on
// Amlogic units, and the BB platform doesn't yet support fleet pool
// rollups.
//
// Source-of-truth is the running firmware's `/api/system/info` response.
// This module is the offline fallback for setup wizard preview and the
// initial render before `systemInfo` arrives.

import type {
  ControllerKind,
  DeviceCapabilityDescriptor,
} from '../api/generated/capability';

/** Canonical platform tier ID. */
export type PlatformTier = "am1-zynq" | "am2-zynq" | "am3-aml" | "am3-bb" | "unknown";

export type PlatformCapabilitySource = 'descriptor' | 'fallback' | 'unknown';

export interface PlatformGate {
  tier: PlatformTier;
  caps: PlatformCapabilities;
  source: PlatformCapabilitySource;
}

/** Capability flags for one platform tier. */
export interface PlatformCapabilities {
  /** FPGA register inspector card visible. */
  fpgaInspector: boolean;
  /** Per-chip serial UART debug console (am3-aml + am3-bb). */
  serialChainConsole: boolean;
  /** dsPIC voltage controller diagnostics (am2 + S19k Pro + am3-bb). */
  dspicDiagnostics: boolean;
  /** PIC16F1704 voltage controller diagnostics (S9 only). */
  pic16f1704Diagnostics: boolean;
  /** TAS5782M voltage DAC diagnostics (S21 / am3-aml NoPic). */
  tas5782mDacDiagnostics: boolean;
  /** APW PMBus telemetry card (only fw bytes that support it). */
  apwPmbusTelemetry: boolean;
  /** Show the Amlogic uImage rootfs install helper. */
  amlogicImageInstaller: boolean;
  /** Show the BB SD-card recovery + uart_trans status card. */
  bbSdCardRecovery: boolean;
  /** Show the EEPROM x21_aes / x19_J / braiinsminer parser tab. */
  hashboardEepromReader: boolean;
  /** Show the autotuner GDTUNER stage indicator ( tune-A). */
  gdtunerIndicator: boolean;
  /** Show the LuxOS-style 21-step profile selector ( tune-B). */
  twentyOneStepProfileSelector: boolean;
  /** Stratum V2 channel state cards (all platforms; data depends on pool). */
  stratumV2Cards: boolean;
}

/** Default-disabled capabilities for an unknown platform — fail closed. */
export const UNKNOWN_PLATFORM_CAPS: PlatformCapabilities = {
  fpgaInspector: false,
  serialChainConsole: false,
  dspicDiagnostics: false,
  pic16f1704Diagnostics: false,
  tas5782mDacDiagnostics: false,
  apwPmbusTelemetry: false,
  amlogicImageInstaller: false,
  bbSdCardRecovery: false,
  hashboardEepromReader: false,
  gdtunerIndicator: false,
  twentyOneStepProfileSelector: false,
  stratumV2Cards: true, // SV2 cards are platform-agnostic.
};

const AM1_ZYNQ: PlatformCapabilities = {
  ...UNKNOWN_PLATFORM_COPY(),
  fpgaInspector: true,
  pic16f1704Diagnostics: true,
};

const AM2_ZYNQ: PlatformCapabilities = {
  ...UNKNOWN_PLATFORM_COPY(),
  fpgaInspector: true,
  dspicDiagnostics: true,
  hashboardEepromReader: true,
};

const AM3_AML: PlatformCapabilities = {
  ...UNKNOWN_PLATFORM_COPY(),
  serialChainConsole: true,
  // S21 is NoPic / TAS5782M; S19k Pro has dsPIC. Both are am3-aml.
  // Show both diagnostics; the runtime API hides whichever isn't applicable.
  dspicDiagnostics: true,
  tas5782mDacDiagnostics: true,
  apwPmbusTelemetry: true,
  amlogicImageInstaller: true,
  hashboardEepromReader: true,
  gdtunerIndicator: true,
  twentyOneStepProfileSelector: true,
};

const AM3_BB: PlatformCapabilities = {
  ...UNKNOWN_PLATFORM_COPY(),
  serialChainConsole: true,
  dspicDiagnostics: true,
  bbSdCardRecovery: true,
  // BB has the BHB42601 EEPROM but no x21_aes parser yet ().
  hashboardEepromReader: false,
  gdtunerIndicator: true,
  twentyOneStepProfileSelector: true,
};

function UNKNOWN_PLATFORM_COPY(): PlatformCapabilities {
  return { ...UNKNOWN_PLATFORM_CAPS };
}

/** Look up capabilities for a platform tier. */
export function platformCapabilities(tier: PlatformTier): PlatformCapabilities {
  switch (tier) {
    case "am1-zynq":
      return AM1_ZYNQ;
    case "am2-zynq":
      return AM2_ZYNQ;
    case "am3-aml":
      return AM3_AML;
    case "am3-bb":
      return AM3_BB;
    default:
      return UNKNOWN_PLATFORM_CAPS;
  }
}

/**
 * Coerce a free-form platform string from `/api/system/info` into a
 * canonical tier ID. Tolerates missing trailing variants (`am3-aml-s19k`
 * collapses to `am3-aml`).
 */
export function tierFromPlatformKey(platformKey: string | undefined): PlatformTier {
  if (!platformKey) return "unknown";
  const normalized = platformKey.toLowerCase().trim();
  if (normalized.startsWith("am1")) return "am1-zynq";
  if (normalized.startsWith("am2")) return "am2-zynq";
  if (normalized.startsWith("am3-aml") || normalized.startsWith("am3aml")) {
    return "am3-aml";
  }
  if (normalized.startsWith("am3-bb") || normalized.startsWith("am3bb")) {
    return "am3-bb";
  }
  return "unknown";
}

/**
 * Advanced/Mining-Hacker hardware tool ids → the capability flag that gates
 * them. A tool whose flag is `false` for the running platform is HIDDEN from
 * the Advanced shell (sidebar, tabbar, overview grid, command palette) and is
 * not reachable via the page-switch — e.g. the FPGA register inspector shells
 * Zynq addresses and returns garbage on Amlogic (am3-aml has no FPGA).
 *
 * Only the bench/hardware NAV_ITEMS appear here. Platform-agnostic tools
 * (console, chip map, protocol, diagnostics, autotuner, journals, exports…)
 * are NOT gated and stay visible on every platform.
 *
 * `uart` is intentionally OR-gated: a chain UART FIFO inspector is meaningful
 * on any platform that has either an FPGA-fronted chain (Zynq) or a direct
 * serial chain console (am3-aml / am3-bb).
 */
export const HARDWARE_TOOL_CAPABILITY: Record<
  string,
  (caps: PlatformCapabilities) => boolean
> = {
  fpga: (caps) => caps.fpgaInspector,
  // i2c/voltage/psu need ANY on-board voltage controller, not specifically a
  // dsPIC: the S9 (am1-zynq) uses a PIC16F1704. The I2C Scanner is the single
  // most S9-relevant hardware tool (it decodes the S9 PIC voltage controllers
  // at 0x55-0x57) — gating these on dspicDiagnostics alone hid them on the S9
  // public-beta gate unit.
  i2c: hasVoltageController,
  voltage: hasVoltageController,
  psu: hasVoltageController,
  uart: (caps) => caps.serialChainConsole || caps.fpgaInspector,
};

/** True for any platform with an on-board voltage controller (dsPIC or PIC16F1704). */
function hasVoltageController(caps: PlatformCapabilities): boolean {
  return caps.dspicDiagnostics || caps.pic16f1704Diagnostics || caps.tas5782mDacDiagnostics;
}

/**
 * True when the given Advanced tool id is visible for these capabilities.
 * Tools not present in `HARDWARE_TOOL_CAPABILITY` are platform-agnostic and
 * always visible. Fails closed: an unknown-platform `caps` set hides every
 * gated hardware tool because all its hardware flags are `false`.
 */
export function isHardwareToolVisible(
  toolId: string,
  caps: PlatformCapabilities,
): boolean {
  const gate = HARDWARE_TOOL_CAPABILITY[toolId];
  return gate ? gate(caps) : true;
}

function normalizedDescriptorTokens(descriptor: DeviceCapabilityDescriptor): string[] {
  return [
    descriptor.identity.platform,
    descriptor.identity.boardTarget,
    descriptor.identity.boardVersion,
    descriptor.board.boardTarget,
    descriptor.board.family,
    descriptor.board.controlBoard,
    descriptor.controlBoard.soc,
    descriptor.controlBoard.controlBoardId,
    descriptor.controlBoard.uioModel,
  ]
    .filter((value): value is string => typeof value === 'string' && value.trim().length > 0)
    .map(value => value.toLowerCase().trim());
}

function descriptorHardwareIsKnown(descriptor: DeviceCapabilityDescriptor): boolean {
  return (
    descriptor.family === 'antminer' &&
    descriptor.support !== 'unknown' &&
    descriptor.support !== 'unsupported' &&
    descriptor.identity.confidence !== 'unknown'
  );
}

function tierFromDescriptorTokens(descriptor: DeviceCapabilityDescriptor): PlatformTier {
  const tokens = normalizedDescriptorTokens(descriptor);
  if (tokens.some(token =>
    token.startsWith('antminer-zynq-am1') ||
    token.startsWith('am1') ||
    token.includes('am1-s9')
  )) {
    return 'am1-zynq';
  }
  if (tokens.some(token =>
    token.startsWith('antminer-zynq-am2') ||
    token.startsWith('am2') ||
    (token.includes('zynq') && token.includes('am2'))
  )) {
    return 'am2-zynq';
  }
  if (tokens.some(token =>
    token.startsWith('antminer-amlogic') ||
    token.startsWith('aml') ||
    token.includes('amlogic') ||
    token.includes('am3-aml')
  )) {
    return 'am3-aml';
  }
  if (tokens.some(token =>
    token.includes('am3-bb') ||
    token.includes('am335') ||
    token.includes('beaglebone') ||
    token.includes('bcb100')
  )) {
    return 'am3-bb';
  }
  return 'unknown';
}

function controllerKinds(descriptor: DeviceCapabilityDescriptor): Set<ControllerKind> {
  return new Set(descriptor.controllers.map(controller => controller.kind));
}

function descriptorVoltageLabel(descriptor: DeviceCapabilityDescriptor): string {
  return (descriptor.power.voltageControl ?? '').toLowerCase();
}

function platformCapabilitiesFromDescriptor(
  descriptor: DeviceCapabilityDescriptor,
  tier: Exclude<PlatformTier, 'unknown'>,
): PlatformCapabilities {
  const caps = { ...platformCapabilities(tier) };
  const kinds = controllerKinds(descriptor);
  const voltageLabel = descriptorVoltageLabel(descriptor);
  const hasControllerDetail = kinds.size > 0 || voltageLabel.length > 0;

  if (hasControllerDetail) {
    caps.pic16f1704Diagnostics =
      kinds.has('pic16f1704') || voltageLabel.includes('pic16');
    caps.dspicDiagnostics =
      kinds.has('dspic33ep') || voltageLabel.includes('dspic');
    caps.tas5782mDacDiagnostics =
      kinds.has('tas-no-pic') || voltageLabel.includes('nopic') || voltageLabel.includes('tas');
  }

  caps.apwPmbusTelemetry =
    caps.apwPmbusTelemetry ||
    descriptor.power.psuMode === 'pmbus-monitor' ||
    Boolean(descriptor.power.psuProtocol?.trim());

  return caps;
}

/**
 * Build the Advanced-dashboard gate from the shared descriptor.
 *
 * A missing descriptor means "older daemon" and may use the legacy
 * `/api/system/info.platform_key` fallback. A present descriptor with unknown,
 * unsupported, or non-Antminer identity fails closed instead of being rescued by
 * the fallback side channel.
 */
export function platformGateFromDeviceCapability(
  descriptor: DeviceCapabilityDescriptor | null | undefined,
  fallbackPlatformKey?: string,
): PlatformGate {
  if (!descriptor) {
    const tier = tierFromPlatformKey(fallbackPlatformKey);
    return {
      tier,
      caps: platformCapabilities(tier),
      source: tier === 'unknown' ? 'unknown' : 'fallback',
    };
  }

  if (!descriptorHardwareIsKnown(descriptor)) {
    return { tier: 'unknown', caps: UNKNOWN_PLATFORM_CAPS, source: 'descriptor' };
  }

  const tier = tierFromDescriptorTokens(descriptor);
  if (tier === 'unknown') {
    return { tier, caps: UNKNOWN_PLATFORM_CAPS, source: 'descriptor' };
  }

  return {
    tier,
    caps: platformCapabilitiesFromDescriptor(descriptor, tier),
    source: 'descriptor',
  };
}
