// @vitest-environment jsdom
//
// FE-1 truth-contract regression: the "Earnings over time" series is a
// RETROACTIVE PROJECTION (current hashrate x the live sats/day estimate applied
// back over time), NOT a record of realized on-chain payouts. It must never be
// labelled as "earned"/realized Bitcoin. This pins the truthful series label
// (the chart's accessible name) and the projection glossary copy.

import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';

import { EarningsChart, type EarningsPoint } from './EarningsChart';
import { GLOSSARY } from '../../utils/glossary';

afterEach(() => cleanup());

const DATA: EarningsPoint[] = [
  { ts: 1_700_000_000_000, sats: 100 },
  { ts: 1_700_000_600_000, sats: 220 },
  { ts: 1_700_000_900_000, sats: 360 },
];

describe('EarningsChart — FE-1 projection (not realized "earned") label', () => {
  it('labels the series as a projection at the current rate, never "earned"', () => {
    render(<EarningsChart period="24h" data={DATA} />);

    // The chart's accessible name IS the series label. It must read as a
    // projection and must NOT use the realized "earned" wording.
    const chart = screen.getByRole('img', { name: /projected sats at current rate/i });
    const name = chart.getAttribute('aria-label') ?? '';
    expect(name).toMatch(/projected/i);
    expect(name).not.toMatch(/earned/i);
  });

  it('uses a neutral "projection" empty state, not an "earnings"/"earned" claim', () => {
    render(<EarningsChart period="24h" data={[]} />);
    const empty = screen.getByTestId('earnings-chart-empty');
    expect(empty.textContent ?? '').not.toMatch(/earned/i);
    expect(empty.textContent ?? '').toMatch(/projection/i);
  });

  it('ships a projection glossary entry that disclaims realized earnings', () => {
    const entry = GLOSSARY.earnings_projection_series;
    expect(entry).toBeTruthy();
    // Honest: explicitly a projection and explicitly NOT realized payouts.
    expect(entry.body).toMatch(/projection/i);
    expect(entry.body.toLowerCase()).toContain('not');
    expect(entry.body.toLowerCase()).toMatch(/payout|credited|on-chain/);
  });
});
