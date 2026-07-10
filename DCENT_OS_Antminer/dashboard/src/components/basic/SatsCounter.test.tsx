// @vitest-environment jsdom
//
// FE-2 truth-contract regression: the USD value next to the sats ticker is an
// ESTIMATE computed from a MANUAL BTC price. DCENT_OS is local-first and never
// fetches a live price, so when the operator has never set one the figure uses
// a built-in fallback default. It must carry an "est." marker and disclose the
// fallback provenance — a hardcoded price is never shown as authoritative.

import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';

import { SatsCounter } from './SatsCounter';
import { useMinerStore } from '../../store/miner';
import type { HeaterStatusResponse } from '../../api/types';

function setReportedSats(sats: number) {
  // sats_today > 0 -> reportedSats drives the visible value; USD renders.
  useMinerStore.setState({
    heaterStatus: { sats_today: sats } as unknown as HeaterStatusResponse,
    status: null,
  });
}

beforeEach(() => {
  // Default fallback posture: built-in price, operator never set one.
  useMinerStore.getState().updateSettings({ btcPrice: 100000, btcPriceLastUpdated: null });
  setReportedSats(50000); // 50_000 sats @ $100k -> $50 USD
});

afterEach(() => {
  cleanup();
  useMinerStore.getState().updateSettings({ btcPrice: 100000, btcPriceLastUpdated: null });
  useMinerStore.setState({ heaterStatus: null, status: null });
});

describe('SatsCounter — FE-2 USD estimate provenance', () => {
  it('marks the USD value as an estimate and names the fallback when no price is set', () => {
    render(<SatsCounter />);

    const usd = document.querySelector('.sats-ticker-usd') as HTMLElement;
    expect(usd).toBeTruthy();
    // Estimate marker + visible fallback disclosure (never a bare "$50.00").
    expect(usd.textContent ?? '').toMatch(/est\./i);
    expect(usd.textContent ?? '').toMatch(/fallback/i);
    expect(usd.getAttribute('data-fallback-price')).toBe('true');
    // The provenance tooltip names the fallback price source.
    expect((usd.getAttribute('title') ?? '').toLowerCase()).toContain('fallback');
  });

  it('still reads as an estimate (no fallback wording) once the operator sets a price', () => {
    useMinerStore.getState().updateSettings({ btcPrice: 95000, btcPriceLastUpdated: Date.now() });
    render(<SatsCounter />);

    const usd = document.querySelector('.sats-ticker-usd') as HTMLElement;
    expect(usd).toBeTruthy();
    expect(usd.textContent ?? '').toMatch(/est\./i);
    // Operator-set: the visible figure no longer advertises a fallback price.
    expect(usd.textContent ?? '').not.toMatch(/fallback/i);
    expect(usd.getAttribute('data-fallback-price')).toBe('false');
  });
});
