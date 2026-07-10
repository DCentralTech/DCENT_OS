// @vitest-environment jsdom

import { act, cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { useMinerStore } from '../../store/miner';
import type { StatusResponse } from '../../api/types';
import { FX_SETTINGS_KEY } from '../../fx/fxSettings';
import { LiveAsicVisual } from './LiveAsicVisual';

function status(hashrateGhs: number): StatusResponse {
  return {
    hashrate_ghs: hashrateGhs,
    hashrate_5s_ghs: hashrateGhs,
    accepted: 0,
    rejected: 0,
    uptime_s: 120,
    firmware_version: 'test',
    mode: 'standard',
    chains: [{
      id: 0,
      chips: 64,
      frequency_mhz: 600,
      voltage_mv: 850,
      temp_c: 55,
      hashrate_ghs: hashrateGhs,
      errors: 0,
      status: hashrateGhs > 0 ? 'active' : 'idle',
    }],
    fans: { pwm: 30, rpm: 3000 },
    pool: {
      url: 'stratum+tcp://pool.example',
      status: hashrateGhs > 0 ? 'mining' : 'standby',
      difficulty: 512,
      last_share_s: 10,
      donating: false,
    },
  };
}

beforeEach(() => {
  window.localStorage.clear();
  Object.defineProperty(document, 'hidden', { configurable: true, value: false });
  useMinerStore.setState({
    status: status(0),
    systemInfo: null,
    wsConnected: false,
    transport: 'rest-polling',
    lastUpdate: Date.now(),
    lastRestPollAt: Date.now(),
    lastWsFrameAt: 0,
  });
});

afterEach(() => {
  cleanup();
});

describe('LiveAsicVisual activity cues', () => {
  it('does not light cells or activity classes for zero hashrate', () => {
    const { container } = render(<LiveAsicVisual />);

    expect(container.querySelector('.live-asic-stack')?.classList.contains('dcfx-asic-active')).toBe(false);
    expect(container.querySelectorAll('.live-asic-chip-grid .is-active')).toHaveLength(0);
    expect(container.querySelectorAll('.dcfx-asic-cell-active')).toHaveLength(0);
  });

  it('uses polled hashrate deltas only on already lit cells', async () => {
    useMinerStore.setState({
      status: status(100),
      lastUpdate: Date.now(),
      lastRestPollAt: Date.now(),
    });
    const { container } = render(<LiveAsicVisual />);

    expect(container.querySelector('.live-asic-stack')?.classList.contains('dcfx-asic-active')).toBe(false);

    act(() => {
      useMinerStore.setState({
        status: status(130),
        lastUpdate: Date.now(),
        lastRestPollAt: Date.now(),
      });
    });

    await waitFor(() => {
      expect(container.querySelector('.live-asic-stack')?.classList.contains('dcfx-asic-active')).toBe(true);
    });

    const pulsedCells = Array.from(container.querySelectorAll('.dcfx-asic-cell-active'));
    expect(pulsedCells.length).toBeGreaterThan(0);
    for (const cell of pulsedCells) {
      expect(cell.classList.contains('is-active')).toBe(true);
    }
  });

  it('does not attach the legacy cell pulse when vitality is calm', () => {
    window.localStorage.setItem(FX_SETTINGS_KEY, JSON.stringify({
      enabled: true,
      vitality: 'calm',
      titleTicker: true,
    }));
    useMinerStore.setState({
      status: status(100),
      lastUpdate: Date.now(),
      lastRestPollAt: Date.now(),
    });

    const { container } = render(<LiveAsicVisual />);

    expect(container.querySelectorAll('.live-asic-chip-grid .is-active').length).toBeGreaterThan(0);
    expect(container.querySelectorAll('[data-pulse="on"]')).toHaveLength(0);
  });
});
