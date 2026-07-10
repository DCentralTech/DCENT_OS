/** @vitest-environment jsdom */

import { cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { FleetResponse } from '../../api/fleet';
import { FleetView } from './FleetView';

const mockFleetApi = vi.hoisted(() => ({
  listFleetMiners: vi.fn(),
}));

vi.mock('../../api/fleet', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../api/fleet')>();
  return {
    ...actual,
    listFleetMiners: mockFleetApi.listFleetMiners,
  };
});

afterEach(() => {
  cleanup();
});

beforeEach(() => {
  vi.clearAllMocks();
});

describe('FleetView telemetry-source honesty', () => {
  it('renders no average temperature measurement when the fleet API returns no rows', async () => {
    const unavailable: FleetResponse = {
      generated_at_ms: Date.UTC(2026, 5, 30, 12, 0, 0),
      source: 'api_unavailable',
      source_label: 'Fleet API unavailable',
      demo: false,
      message: 'Fleet endpoint unavailable. Demo miners are hidden.',
      miners: [],
    };
    mockFleetApi.listFleetMiners.mockResolvedValueOnce(unavailable);

    render(<FleetView />);

    const notice = within(await screen.findByTestId('fleet-source-notice'));
    expect(notice.getByText('Fleet API unavailable')).toBeTruthy();
    const summary = within(screen.getByTestId('fleet-summary'));
    const averageTempLabel = summary.getByText('Average Temp');
    expect(averageTempLabel).toBeTruthy();
    expect(averageTempLabel.closest('.fleet-summary-card')?.textContent).toContain('—');
    expect(screen.queryByText('0.0 C')).toBeNull();
  });

  it('labels demo fleet rows as a static fixture, not live telemetry', async () => {
    const demoFleet: FleetResponse = {
      generated_at_ms: Date.UTC(2026, 5, 30, 12, 0, 0),
      source: 'demo',
      source_label: 'Demo fixture',
      demo: true,
      miners: [
        {
          id: 'demo-s9',
          hostname: 'demo-s9',
          ip: '192.0.2.10',
          model: 'Antminer S9',
          hashrate_ghs: 13_500,
          temp_c: 52,
          fan_pwm: 25,
          status: 'alive',
          last_seen_ms: Date.UTC(2026, 5, 30, 11, 59, 0),
        },
      ],
    };
    mockFleetApi.listFleetMiners.mockResolvedValueOnce(demoFleet);

    render(<FleetView />);

    expect(await screen.findByText('Demo fleet data')).toBeTruthy();
    expect(screen.getByText('This is a static demo fixture, not live miner telemetry.')).toBeTruthy();
    expect(screen.getByTestId('fleet-row-demo-s9')).toBeTruthy();
  });
});
