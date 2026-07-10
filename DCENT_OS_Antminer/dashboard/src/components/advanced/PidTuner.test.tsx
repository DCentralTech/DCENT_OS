/** @vitest-environment jsdom */

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { PidTuner } from './PidTuner';

const mockApi = vi.hoisted(() => ({
  getPidState: vi.fn(),
  setPidParams: vi.fn(),
}));

vi.mock('../../api/client', () => ({
  api: mockApi,
}));

let mockIsProxyMode = false;
vi.mock('../common/proxy/SystemHealthContext', () => ({
  useSystemHealth: () => ({ isProxyMode: mockIsProxyMode }),
}));

beforeEach(() => {
  vi.clearAllMocks();
  mockIsProxyMode = false;
  mockApi.getPidState.mockResolvedValue({
    kp: 2,
    ki: 0.1,
    kd: 0.5,
    setpoint: 55,
    current_temp: 53.2,
    output: 27,
    integral: 1.5,
    last_error: 0.4,
  });
});

afterEach(() => {
  cleanup();
});

describe('PidTuner live-data honesty', () => {
  it('does not simulate PID telemetry and disables Apply until live state is read', () => {
    render(<PidTuner />);

    expect(screen.queryByText(/simulated/i)).toBeNull();
    expect(screen.getAllByText('AWAITING LIVE STATE').length).toBeGreaterThan(0);
    expect(screen.getByText(/Read current PID state to begin/i)).toBeTruthy();
    expect((screen.getByRole('button', { name: 'Apply' }) as HTMLButtonElement).disabled).toBe(true);
    expect(mockApi.setPidParams).not.toHaveBeenCalled();
  });
});
