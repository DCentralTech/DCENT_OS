// @vitest-environment jsdom
//
// STD-A-09 honesty regression: the Earnings page falls back to a nominal ~25 W
// "standby" figure when the daemon reports no wall-power telemetry. That assumed
// value must NOT be presented as a real reading — the Power Draw card renders an
// em-dash + "standby (assumed)" and the calculator's "(live)" tag is suppressed
// unless the watts came from measured telemetry.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';

// EarningsPage polls /api/perf/efficiency and (via useNetworkContext)
// /api/network/block. Both are wrapped in try/catch, so rejecting keeps the
// component on its null-telemetry render path — exactly the state under test.
vi.mock('../../api/client', () => ({
  api: {
    getPerfEfficiency: vi.fn().mockRejectedValue(new Error('no perf endpoint')),
    getNetworkBlock: vi.fn().mockRejectedValue(new Error('no network block')),
  },
}));

import { EarningsPage } from './EarningsPage';
import { useMinerStore } from '../../store/miner';
import type { StatusResponse, StatsResponse } from '../../api/types';

function setStore(opts: {
  status: Partial<StatusResponse> | null;
  stats: Partial<StatsResponse> | null;
}) {
  useMinerStore.setState({
    status: (opts.status as unknown as StatusResponse) ?? null,
    stats: (opts.stats as unknown as StatsResponse) ?? null,
  });
}

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => {
  cleanup();
  useMinerStore.setState({ status: null, stats: null });
});

describe('EarningsPage — STD-A-09 Power Draw honesty', () => {
  it('renders an em-dash + "standby (assumed)" and no "(live)" tag when power telemetry is absent', () => {
    // status present (so the page renders, not the first-load skeleton) but no
    // stats.power, and standby (hashrate 0) → watts is the assumed ~25 W fallback.
    setStore({ status: { hashrate_ghs: 0, uptime_s: 0 }, stats: null });
    render(<EarningsPage />);

    expect(screen.getByText('standby (assumed)')).toBeTruthy();
    // The assumed value is never advertised as an authoritative "(live)" reading.
    expect(screen.queryByText('(live)')).toBeNull();
  });

  it('shows the measured watts with a "(live)" tag when power telemetry is present', () => {
    setStore({
      status: { hashrate_ghs: 0, uptime_s: 0 },
      stats: {
        power: {
          wall_watts: 1350,
          source: 'pmbus',
          source_detail: 'pmbus_measured',
          live_power_available: true,
        },
      },
    });
    render(<EarningsPage />);

    // Real telemetry → no "assumed" disclaimer, and the calculator "(live)" tag shows.
    expect(screen.queryByText('standby (assumed)')).toBeNull();
    expect(screen.getByText('(live)')).toBeTruthy();
  });

  it('does not treat static fallback watts as live profitability power', () => {
    setStore({
      status: { hashrate_ghs: 0, uptime_s: 0 },
      stats: {
        power: {
          wall_watts: 1350,
          watts: 1200,
          source: 'static_model_fallback',
          live_power_available: false,
          modeled: true,
          btu_h: 4606,
        },
      },
    });
    render(<EarningsPage />);

    expect(screen.getByText('standby (assumed)')).toBeTruthy();
    expect(screen.queryByText('(live)')).toBeNull();
  });

  it('does not render legacy fallback efficiency without live power provenance', () => {
    setStore({
      status: { hashrate_ghs: 0, uptime_s: 0 },
      stats: {
        power: {
          wall_watts: 1350,
          efficiency_jth: 33.5,
          source: 'static_model_fallback',
          source_detail: 'static_power_fallback_from_miner_state',
          live_power_available: false,
          modeled: true,
        },
      },
    });
    render(<EarningsPage />);

    expect(screen.queryByTestId('efficiency-jth-value')).toBeNull();
  });

  it('renders legacy efficiency when the same power object is live-provenance', () => {
    setStore({
      status: { hashrate_ghs: 0, uptime_s: 0 },
      stats: {
        power: {
          wall_watts: 1350,
          efficiency_jth: 33.5,
          source: 'pmbus',
          source_detail: 'pmbus_measured',
          live_power_available: true,
        },
      },
    });
    render(<EarningsPage />);

    expect(screen.getByTestId('efficiency-jth-value').textContent).toContain('33.5 J/TH');
  });
});
