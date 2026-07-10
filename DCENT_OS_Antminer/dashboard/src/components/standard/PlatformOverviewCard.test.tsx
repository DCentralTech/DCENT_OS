/** @vitest-environment jsdom */

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { useMinerStore } from '../../store/miner';
import { PlatformOverviewCard } from './PlatformOverviewCard';

vi.mock('../../hooks/useModelProfiles', () => ({
  useModelProfiles: () => ({ liveChips: null, fellBackToSnapshot: true }),
}));

afterEach(() => {
  cleanup();
  useMinerStore.setState({ systemInfo: null, status: null, mode: 'standard' });
});

describe('PlatformOverviewCard customer-facing tier copy', () => {
  it('uses product-grade development copy for non-beta model profiles', () => {
    useMinerStore.setState({
      systemInfo: {
        model: 'Antminer S17',
        chip_type: 'BM1397',
        chain_count: 3,
        chip_count: 144,
      } as never,
      status: { hashrate_ghs: 0 } as never,
      mode: 'standard',
    });

    render(<PlatformOverviewCard />);

    expect(screen.getByTestId('platform-overview-development-pill').textContent).toContain(
      'Experimental',
    );
    expect(screen.getByTestId('platform-overview-development-note').textContent).toContain(
      'Experimental profile',
    );
    expect(screen.queryByText(/unvalidated/i)).toBeNull();
    expect(screen.queryByText(/not yet validated/i)).toBeNull();
  });

  it('renders registered hardware-gated models without fabricating chip geometry', () => {
    useMinerStore.setState({
      systemInfo: {
        model: 'Antminer T19',
        chip_type: 'BM1398',
        chain_count: 3,
        chip_count: null,
      } as never,
      status: { hashrate_ghs: 0 } as never,
      mode: 'standard',
    });

    render(<PlatformOverviewCard />);

    expect(screen.getByTestId('platform-overview-display-name').textContent).toContain(
      'Antminer T19',
    );
    expect(screen.getByText('3 chains; chip count in development')).toBeTruthy();
    expect(screen.queryByTestId('platform-overview-unregistered-pill')).toBeNull();
    expect(screen.queryByText(/3 × 0/)).toBeNull();
  });
});
