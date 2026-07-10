// @vitest-environment jsdom
//
// FEUX-1 (P2) regression: the per-chain chip-voltage range slider in
// VoltageControl is safety-critical (its value is the raw PIC DAC code and an
// out-of-range code can permanently damage the hash board). It MUST expose a
// non-empty accessible name so screen-reader users know what the slider drives
// before they move it. This pins the aria-label / aria-valuetext added in the
//  frontend a11y pass and fails if a future edit drops them.
//
// We assert the accessible name two ways: getAllByRole('slider') to count the
// controls, and getByRole('slider', { name }) — which computes the accessible
// name via the same dom-accessibility-api engine a screen reader uses and
// throws if the name is missing/empty.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { VoltageControl } from './VoltageControl';
import { useMinerStore } from '../../store/miner';

// Keep the on-mount live-PIC read from hitting the network. The slider renders
// from default state regardless, but mocking keeps the test deterministic.
vi.mock('../../api/client', () => ({
  api: {
    readI2c: vi.fn().mockResolvedValue({ data: [] }),
    setChipVoltage: vi.fn().mockResolvedValue({ status: 'ok' }),
  },
}));

beforeEach(() => {
  vi.clearAllMocks();
  // HACK-B-005: the interactive per-chip voltage sliders are S9 (am1-zynq /
  // PIC16F1704) specific and now only render on an S9 platform — on other
  // controllers the tool shows an honest "not available" state instead of
  // mis-programming dsPIC/TAS5782M hardware. Put the store in an S9 state so the
  // a11y contract under test (the sliders themselves) is exercised.
  useMinerStore.setState({ systemInfo: { platform_key: 'am1-s9' } as never });
});

afterEach(() => {
  cleanup();
  useMinerStore.setState({ systemInfo: null });
});

describe('VoltageControl accessibility (FEUX-1)', () => {
  it('renders one chip-voltage slider per chain, each with a non-empty accessible name', () => {
    render(<VoltageControl />);

    const sliders = screen.getAllByRole('slider');
    // Default state has 3 chains (6/7/8).
    expect(sliders.length).toBe(3);

    for (const slider of sliders) {
      // aria-label is the accessible-name source for these sliders.
      const ariaLabel = slider.getAttribute('aria-label') ?? '';
      expect(ariaLabel.trim().length).toBeGreaterThan(0);
      expect(ariaLabel.toLowerCase()).toContain('voltage');
    }

    // Accessible-name query (dom-accessibility-api): throws if any chain
    // slider lacks a computable name. Covers all three default chains.
    expect(screen.getByRole('slider', { name: /chain 6 chip voltage/i })).toBeTruthy();
    expect(screen.getByRole('slider', { name: /chain 7 chip voltage/i })).toBeTruthy();
    expect(screen.getByRole('slider', { name: /chain 8 chip voltage/i })).toBeTruthy();
  });

  it('exposes a descriptive aria-valuetext on the chip-voltage slider', () => {
    render(<VoltageControl />);

    const sliders = screen.getAllByRole('slider');
    for (const slider of sliders) {
      const valueText = slider.getAttribute('aria-valuetext') ?? '';
      // aria-valuetext should surface the decoded voltage + zone, not just the
      // raw DAC integer (which a sighted user reads off the readout).
      expect(valueText).toMatch(/PIC \d+/);
      expect(valueText).toContain('V');
    }
  });
});
