// @vitest-environment jsdom
//
// FEUX-2 (P3) regression: the voltage controls in the PSU Override / PSU Control
// modals used bare <label> elements with no htmlFor association, so their
// rendered <input>/<select> had no computed accessible name. These are
// safety-relevant (they set the PSU rail / APW target voltage). This pins the
// htmlFor + id association added in the  frontend a11y pass.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { PsuOverrideModal, PsuControlModal } from './HardwareInfo';
import { useMinerStore } from '../../store/miner';
import type { PsuOverrideModel, PsuTroubleshootResponse } from '../../api/types';

const troubleshootPsu = vi.fn();
const controlPsu = vi.fn();

vi.mock('../../api/client', () => ({
  api: {
    troubleshootPsu: (...args: unknown[]) => troubleshootPsu(...args),
    controlPsu: (...args: unknown[]) => controlPsu(...args),
    updatePsuOverride: vi.fn().mockResolvedValue({ status: 'ok', message: 'Saved' }),
  },
}));

const MODELS: PsuOverrideModel[] = [
  { id: 'APW3', name: 'APW3', voltage_range: '12.0V - 14.5V' },
  { id: 'APW7', name: 'APW7', voltage_range: '12.0V - 15.0V' },
];

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => {
  cleanup();
  // Reset the locally-mutated UI mode so this file does not leak state.
  useMinerStore.setState({ mode: 'standard' });
});

describe('PsuOverrideModal accessibility (FEUX-2)', () => {
  it('associates the Fixed Output Voltage slider with its label', () => {
    render(
      <PsuOverrideModal
        onClose={() => {}}
        availableModels={MODELS}
        currentModel="APW7"
        currentActive={true}
        currentVoltage={12.0}
      />,
    );

    // Accessible name is computed from the now-associated <label htmlFor>.
    // getByRole({ name }) throws if the computed accessible name is missing,
    // so a successful match IS the non-empty-name assertion.
    const slider = screen.getByRole('slider', { name: /fixed output voltage/i });
    expect(slider).toBeTruthy();
  });

  it('associates the PSU Model select with its label', () => {
    render(
      <PsuOverrideModal
        onClose={() => {}}
        availableModels={MODELS}
        currentModel="APW7"
        currentActive={true}
        currentVoltage={12.0}
      />,
    );

    const select = screen.getByRole('combobox', { name: /psu model/i });
    expect(select).toBeTruthy();
  });
});

describe('PsuControlModal accessibility (FEUX-2)', () => {
  it('associates the Target Voltage input with its label', async () => {
    // Hacker mode + a PSU that supports voltage set so the input renders.
    useMinerStore.setState({ mode: 'hacker' });
    const diag: PsuTroubleshootResponse = {
      detected: true,
      model: 'APW12',
      control_mode: 'pmbus',
      voltage_range: '12.00V - 15.00V',
      voltage_out: 13.8,
      supports_voltage_set: true,
      message: 'OK',
    };
    troubleshootPsu.mockResolvedValue(diag);

    render(<PsuControlModal onClose={() => {}} />);

    // The input is gated behind the async troubleshoot fetch; wait for it.
    // findByRole({ name }) resolves only if the computed accessible name
    // matches — i.e. the label is now associated and the name is non-empty.
    const input = await screen.findByRole('spinbutton', { name: /target voltage/i });
    expect(input).toBeTruthy();
  });
});
