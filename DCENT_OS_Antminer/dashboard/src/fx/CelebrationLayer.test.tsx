// @vitest-environment jsdom

import React from 'react';
import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { FxEvent } from './rewardBus';

let rewardHandler: ((event: FxEvent) => void) | null = null;

vi.mock('./useRewardFx', () => ({
  useRewardFx: (handler: (event: FxEvent) => void) => {
    rewardHandler = handler;
  },
}));

import { CelebrationLayer } from './CelebrationLayer';

beforeEach(() => {
  vi.useFakeTimers();
  rewardHandler = null;
  window.localStorage.clear();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  rewardHandler = null;
  window.localStorage.clear();
});

describe('CelebrationLayer', () => {
  it('renders a real lucky-share moment with achieved and target difficulty', () => {
    render(<CelebrationLayer />);
    expect(screen.getByTestId('dcfx-layer')).toBeTruthy();

    act(() => {
      rewardHandler?.({
        kind: 'lucky-share',
        at: 1_700_000_000_000,
        intensity: 1,
        difficulty: 12_482,
        targetDifficulty: 512,
      });
    });

    expect(screen.getByText('Lucky share')).toBeTruthy();
    expect(screen.getByText('12,482 achieved / 512 target')).toBeTruthy();
    expect(document.querySelectorAll('.dcfx-dot')).toHaveLength(12);

    act(() => {
      vi.advanceTimersByTime(2300);
    });

    expect(screen.queryByText('Lucky share')).toBeNull();
  });

  it('keeps cap-overflow best difficulty static without a moment animation', () => {
    render(<CelebrationLayer />);

    act(() => {
      rewardHandler?.({
        kind: 'best-difficulty',
        at: 1_700_000_000_000,
        intensity: 0,
        difficulty: 2048,
        targetDifficulty: 512,
      });
    });

    expect(screen.getByText('Session best: 2,048')).toBeTruthy();
    expect(screen.queryByText('New session best')).toBeNull();
  });
});
