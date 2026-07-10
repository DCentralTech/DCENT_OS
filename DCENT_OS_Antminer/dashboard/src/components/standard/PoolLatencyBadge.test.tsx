// @vitest-environment jsdom
//
// LANE-D regression: the per-pool latency badge in the Active Pool Health list
// renders the measured RTT for the active pool and an honest "—" when there is
// no measurement (inactive pool / never-observed honest_default). It must never
// show a fabricated "0 ms" for a pool that was never measured.

import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';

import { PoolLatencyBadge } from './PoolLatencyBadge';

afterEach(() => cleanup());

describe('PoolLatencyBadge — honest per-pool latency', () => {
  it('renders the measured value in ms for an active, real measurement', () => {
    render(
      <PoolLatencyBadge
        latencyMs={42}
        latencyMeasured={true}
        latencyMsSource="stratum_status"
        poolId={0}
      />,
    );
    expect(screen.getByText('42 ms')).toBeTruthy();
    expect(screen.queryByText('—')).toBeNull();
  });

  it('renders "—" when latency is null (inactive pool, never measured)', () => {
    render(
      <PoolLatencyBadge
        latencyMs={null}
        latencyMeasured={false}
        poolId={1}
      />,
    );
    expect(screen.getByText('—')).toBeTruthy();
    expect(screen.queryByText(/ms$/)).toBeNull();
  });

  it('renders "—" for a 0-ms honest_default placeholder (never a fake 0 ms)', () => {
    render(
      <PoolLatencyBadge
        latencyMs={0}
        latencyMeasured={true}
        latencyMsSource="honest_default"
        poolId={2}
      />,
    );
    expect(screen.getByText('—')).toBeTruthy();
    expect(screen.queryByText('0 ms')).toBeNull();
  });

  it('rounds a fractional measured RTT to whole milliseconds', () => {
    render(
      <PoolLatencyBadge
        latencyMs={127.6}
        latencyMeasured={true}
        latencyMsSource="stratum_status"
        poolId={3}
      />,
    );
    expect(screen.getByText('128 ms')).toBeTruthy();
  });
});
