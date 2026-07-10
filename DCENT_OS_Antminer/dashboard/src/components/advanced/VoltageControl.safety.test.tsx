// @vitest-environment jsdom
//
// SAFETY-GATE regression pin for VoltageControl — the most physically dangerous
// control in the dashboard (it writes raw PIC DAC codes; an out-of-range code
// can permanently damage a hash board). The component is correctly defended
// three ways, but only its slider accessible-NAME was previously tested
// (VoltageControl.a11y.test.tsx), so a refactor could silently drop a defense.
//
// This file is TEST-ONLY: it does NOT change VoltageControl.tsx. It pins the
// three live safety behaviors against the actual source so they can't regress:
//   (1) HACK-B-005 non-S9 fail-closed — on any non-S9 controller the tool
//       renders an honest "unavailable" state with NO slider and NO Apply, so
//       the S9-only DAC formula/addresses can't mis-program other hardware.
//   (2) Safe-Lock clamp — locked sliders floor at PIC_MIN (40), not 0, and the
//       Apply button hard-disables when a chain's code decodes outside 7.5–9.5V.
//   (3) Proxy/hybrid block — when bosminer owns the voltage hardware, the write
//       is blocked (banner + disabled Apply).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';

import { VoltageControl } from './VoltageControl';
import { useMinerStore } from '../../store/miner';

// Keep the on-mount live-PIC read off the network (mirror the a11y test). The
// safety contracts under test render from default state regardless.
vi.mock('../../api/client', () => ({
  api: {
    readI2c: vi.fn().mockResolvedValue({ data: [] }),
    setChipVoltage: vi.fn().mockResolvedValue({ estimated_voltage_v: 9.1 }),
  },
}));

// Drive isProxyMode without standing up the real SystemHealthProvider (which
// fetches /api/system/health). Mutable holder, vitest `mock`-prefix hoisting
// rule — same pattern as BootPhaseBanner.test.tsx.
let mockIsProxyMode = false;
vi.mock('../common/proxy/SystemHealthContext', () => ({
  useSystemHealth: () => ({ isProxyMode: mockIsProxyMode }),
}));

beforeEach(() => {
  vi.clearAllMocks();
  mockIsProxyMode = false;
});

afterEach(() => {
  cleanup();
  useMinerStore.setState({ systemInfo: null });
});

describe('VoltageControl safety gates (regression pin)', () => {
  it('(1) HACK-B-005: fails closed on a non-S9 platform — no slider, no Apply', () => {
    // tierFromPlatformKey('am3-aml') !== 'am1-zynq' → isS9 false.
    useMinerStore.setState({ systemInfo: { platform_key: 'am3-aml' } as never });
    render(<VoltageControl />);

    // No interactive voltage programming surface at all.
    expect(screen.queryAllByRole('slider').length).toBe(0);
    expect(screen.queryByRole('button', { name: /apply voltage/i })).toBeNull();

    // The honest "S9 only / unavailable" copy renders instead.
    expect(screen.getByText('S9 ONLY')).toBeTruthy();
    expect(screen.getByText(/Per-chip voltage control unavailable/i)).toBeTruthy();
  });

  it('(2) Safe-Lock: locked sliders floor at PIC_MIN (40) not 0, and Apply hard-disables on an out-of-range code', () => {
    useMinerStore.setState({ systemInfo: { platform_key: 'am1-s9' } as never });
    render(<VoltageControl />);

    // Safe Lock is on by default → each chain slider is clamped to the locked
    // DAC band [PIC_MIN=40, PIC_MAX=200], never the full 0–255 range.
    const sliders = screen.getAllByRole('slider');
    expect(sliders.length).toBe(3);
    for (const s of sliders) {
      expect(s.getAttribute('min')).toBe('40');
      expect(s.getAttribute('max')).toBe('200');
    }

    // Drive chain 6 into an unsafe code: unlock to reach the full DAC range,
    // set a code that decodes below 7.5V (PIC 400 → ~7.09V), then re-lock.
    // With Safe Lock back on AND an out-of-range code, Apply is hard-disabled
    // (belt-and-suspenders over the danger-confirm dialog).
    fireEvent.click(screen.getByRole('button', { name: /unlock/i }));
    const numericInputs = screen.getAllByRole('spinbutton'); // type=number
    fireEvent.change(numericInputs[0], { target: { value: '400' } });
    fireEvent.click(screen.getByRole('button', { name: /lock/i }));

    const applyButtons = screen.getAllByRole('button', { name: /apply voltage/i });
    expect(applyButtons.length).toBe(3);
    // Chain 6 (unsafe) is disabled; chain 7 (default safe code 100) is not —
    // proving the disable is value-driven, not a blanket lock.
    expect((applyButtons[0] as HTMLButtonElement).disabled).toBe(true);
    expect((applyButtons[1] as HTMLButtonElement).disabled).toBe(false);
  });

  it('(3) proxy/hybrid: blocks the voltage write when bosminer owns the hardware', () => {
    mockIsProxyMode = true;
    useMinerStore.setState({ systemInfo: { platform_key: 'am1-s9' } as never });
    render(<VoltageControl />);

    // Honest "writes disabled" banner renders…
    expect(screen.getByText(/bosminer owns hardware in proxy\/hybrid mode/i)).toBeTruthy();

    // …and every Apply button is disabled.
    const applyButtons = screen.getAllByRole('button', { name: /apply voltage/i });
    expect(applyButtons.length).toBe(3);
    for (const b of applyButtons) {
      expect((b as HTMLButtonElement).disabled).toBe(true);
    }
  });
});
