/** @vitest-environment jsdom */

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { useWindowedList } from './useWindowedList';

function Harness({ count, disabled = false }: { count: number; disabled?: boolean }) {
  const list = useWindowedList<HTMLDivElement>({
    count,
    itemHeight: 10,
    overscan: 2,
    disabled,
  });

  return (
    <div ref={list.containerRef} onScroll={list.onScroll} data-testid="scroller">
      <span data-testid="range">
        {list.start}:{list.end}:{list.padTop}:{list.padBottom}
      </span>
    </div>
  );
}

describe('useWindowedList', () => {
  afterEach(() => {
    cleanup();
  });

  it('renders every item when disabled', () => {
    render(<Harness count={100} disabled />);

    expect(screen.getByTestId('range').textContent).toBe('0:100:0:0');
  });

  it('returns a bounded initial window for large lists', () => {
    render(<Harness count={100} />);

    expect(screen.getByTestId('range').textContent).toBe('0:16:0:840');
  });
});
