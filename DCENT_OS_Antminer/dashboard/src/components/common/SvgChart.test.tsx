// @vitest-environment jsdom

import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { SvgChart, type ChartSeries } from './SvgChart';

function series(values: number[]): ChartSeries[] {
  return [{
    label: 'Hashrate',
    color: '#FAA500',
    data: values.map((value, index) => ({ time: index + 1, value })),
  }];
}

afterEach(() => {
  cleanup();
});

describe('SvgChart draw-in guard', () => {
  it('draws on first identity only, not on polling data growth', () => {
    const { container, rerender } = render(
      <SvgChart series={series([1, 2])} staleAfterSec={0} />,
    );

    expect(container.querySelector('.svgchart-line')?.getAttribute('data-draw')).toBe('true');

    rerender(<SvgChart series={series([1, 2, 3])} staleAfterSec={0} />);

    expect(container.querySelector('.svgchart-line')?.hasAttribute('data-draw')).toBe(false);
  });

  it('draws once for an explicit new draw identity', () => {
    const { container, rerender } = render(
      <SvgChart series={series([1, 2])} staleAfterSec={0} drawIdentity="window-a" />,
    );

    expect(container.querySelector('.svgchart-line')?.getAttribute('data-draw')).toBe('true');

    rerender(<SvgChart series={series([2, 3])} staleAfterSec={0} drawIdentity="window-b" />);

    expect(container.querySelector('.svgchart-line')?.getAttribute('data-draw')).toBe('true');
  });
});
