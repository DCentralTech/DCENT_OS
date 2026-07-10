/** @vitest-environment jsdom */

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { OffGridPage, getThresholdWarnings } from './OffGridPage';

const mockApi = vi.hoisted(() => ({
  getOffGridStatus: vi.fn(),
  getOffGridPresets: vi.fn(),
  getOffGridConfig: vi.fn(),
  updateOffGridConfig: vi.fn(),
  testOffGridConfig: vi.fn(),
  getSolarStatus: vi.fn(),
  getSolarVerificationHistory: vi.fn(),
}));

const mockStore = vi.hoisted(() => ({
  addToast: vi.fn(),
}));

vi.mock('../../api/client', () => ({
  api: mockApi,
}));

vi.mock('../../store/miner', () => ({
  useMinerStore: (selector: (state: { addToast: typeof mockStore.addToast }) => unknown) =>
    selector({ addToast: mockStore.addToast }),
}));

vi.mock('../common/Tooltip', () => ({
  InfoDot: () => null,
}));

const status = {
  enabled: false,
  zone: 'normal',
  state: 'disabled',
  bus_voltage_v: 52,
  current_a: 0,
  power_w: 0,
  battery_soc_pct: 80,
  target_freq_mhz: 0,
  freq_pct: 0,
  voltage_rate_vps: 0,
  uptime_battery_s: 0,
  energy_consumed_wh: 0,
  sensor_source: 'Simulated',
  has_current: true,
  sensor_ok: true,
  message: 'disabled',
};

const simulatedConfig = {
  source_profile: 'direct_dc',
  enabled: false,
  battery_preset: 'lifepo4_48v',
  adc: { type: 'simulated', voltage_v: 52, current_a: 0 },
  freq_step_mhz: 25,
  min_frequency_mhz: 200,
  loop_interval_ms: 2000,
  custom_critical_v: null,
  custom_low_v: null,
  custom_high_v: null,
  custom_full_v: null,
  custom_recovery_v: null,
  ready: false,
  restart_required: true,
  readiness_message: 'Simulated ADC is lab-only and cannot arm off-grid protection.',
} as const;

beforeEach(() => {
  vi.clearAllMocks();
  mockApi.getOffGridStatus.mockResolvedValue(status);
  mockApi.getOffGridPresets.mockResolvedValue({
    presets: [{
      id: 'lifepo4_48v',
      label: 'LiFePO4 48V',
      critical_v: 40,
      low_v: 47,
      normal_v: 52,
      high_v: 53.6,
      full_v: 54.4,
      recovery_v: 50,
    }],
  });
  mockApi.getOffGridConfig.mockResolvedValue(simulatedConfig);
  mockApi.getSolarStatus.mockResolvedValue(null);
  mockApi.getSolarVerificationHistory.mockResolvedValue({ entries: [] });
  mockApi.testOffGridConfig.mockResolvedValue({
    ok: true,
    backend: 'simulated',
    sensorSource: 'Simulated',
    hasCurrent: true,
    plausible: true,
    voltageV: 52,
    currentA: 0,
    powerW: 0,
    message: 'Simulated ADC path responded.',
  });
});

afterEach(() => {
  cleanup();
});

describe('OffGridPage simulated ADC safety', () => {
  it('allows lab probing but blocks saving simulated ADC as protection config', async () => {
    render(<OffGridPage />);

    expect(await screen.findByText(/cannot be saved as off-grid protection config/i)).toBeTruthy();
    const save = screen.getByRole('button', { name: /save off-grid config/i }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);

    fireEvent.click(save);
    expect(mockApi.updateOffGridConfig).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole('button', { name: /test adc path/i }));
    await waitFor(() => expect(mockApi.testOffGridConfig).toHaveBeenCalledTimes(1));
    expect(mockApi.testOffGridConfig).toHaveBeenCalledWith(expect.objectContaining({
      adc: expect.objectContaining({ type: 'simulated' }),
    }));
  });

  it('labels BTU output as estimated when current telemetry is not measured', async () => {
    mockApi.getOffGridStatus.mockResolvedValue({
      ...status,
      enabled: true,
      state: 'running',
      power_w: 1000,
      has_current: false,
    });

    render(<OffGridPage />);

    expect(await screen.findByText('BTU/h est.')).toBeTruthy();
    expect(await screen.findByText('3412')).toBeTruthy();
    expect((await screen.findAllByText('Estimated from source telemetry')).length).toBeGreaterThanOrEqual(2);
  });
});

describe('getThresholdWarnings (D7-4 dangerous-threshold Save gate)', () => {
  // Save is disabled whenever this returns a non-empty list, so a self-flagged
  // dangerous/incomplete battery-threshold set can no longer be committed.
  type Cfg = Parameters<typeof getThresholdWarnings>[0];
  const cfg = (o: Record<string, number>): Cfg => o as unknown as Cfg;
  const complete = {
    custom_critical_v: 44,
    custom_low_v: 46,
    custom_high_v: 50,
    custom_full_v: 54,
    custom_recovery_v: 48,
  };

  it('flags recovery <= critical (permanent-sleep risk)', () => {
    const w = getThresholdWarnings(cfg({ ...complete, custom_recovery_v: 43 }), undefined);
    expect(w.length).toBeGreaterThan(0);
    expect(w.join(' ')).toMatch(/recovery/i);
  });

  it('flags a non-rising critical<low<high<full order', () => {
    const w = getThresholdWarnings(cfg({ ...complete, custom_critical_v: 51 }), undefined);
    expect(w.length).toBeGreaterThan(0);
  });

  it('flags incomplete thresholds', () => {
    const w = getThresholdWarnings(cfg({ custom_critical_v: 44 }), undefined);
    expect(w.length).toBeGreaterThan(0);
  });

  it('is empty for a cleanly-rising set (Save allowed)', () => {
    expect(getThresholdWarnings(cfg(complete), undefined)).toEqual([]);
  });
});
