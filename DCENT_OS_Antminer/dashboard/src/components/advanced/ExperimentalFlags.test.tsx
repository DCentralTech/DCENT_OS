// @vitest-environment jsdom
//
// FE-DEAD-2 / SLOP-TOOL-06 regression: the Experimental "Lab Switches" flags are
// waiting on daemon integration. Rather than ship a full panel of permanently-
// disabled toggles, when NO flag is available the panel collapses to a single honest
// "none available in this build" note + a NONE AVAILABLE pill — so there is no
// surface of dead-but-live-looking controls in the public beta. The full toggle
// grid returns automatically the moment any flag flips `comingSoon: false`.

import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';

import {
  countEnabledAvailableFlags,
  ExperimentalFlags,
  normalizeExperimentalFlagState,
  type FlagDef,
} from './ExperimentalFlags';

beforeEach(() => localStorage.clear());
afterEach(() => {
  cleanup();
  localStorage.clear();
});

function flagToggles(): HTMLButtonElement[] {
  // queryAllByRole (not getAllByRole) so an honest-empty state with zero buttons
  // returns [] instead of throwing.
  return screen
    .queryAllByRole('button')
    .filter((b) => b.className.includes('xf-toggle')) as HTMLButtonElement[];
}

describe('ExperimentalFlags — FE-DEAD-2 / SLOP-TOOL-06 coming-soon honesty', () => {
  it('renders an honest "none available" empty state when no flag is daemon-available', () => {
    render(<ExperimentalFlags />);
    // Current build: every FlagDef is comingSoon -> the panel must NOT render a
    // grid of inert toggles; it shows a single honest note + NONE AVAILABLE pill.
    expect(screen.getByText('NONE AVAILABLE')).toBeTruthy();
    expect(screen.getByText(/None available in this build/i)).toBeTruthy();
  });

  it('renders no interactive flag toggles, so nothing can be mistaken for a live control', () => {
    render(<ExperimentalFlags />);
    expect(flagToggles()).toHaveLength(0);
    // No "Coming soon" badge grid and no live-looking enabled-count chip in the empty state.
    expect(screen.queryAllByText('Coming soon')).toHaveLength(0);
  });

  it('drops stale localStorage state for roadmap-only flags', () => {
    const defs: FlagDef[] = [
      { key: 'available', label: 'Available Flag', description: 'Ready', comingSoon: false },
      { key: 'roadmap', label: 'Roadmap Flag', description: 'Future' },
    ];

    const normalized = normalizeExperimentalFlagState(
      { available: true, roadmap: true, deleted_flag: true },
      defs,
    );

    expect(normalized).toEqual({ available: true });
    expect(countEnabledAvailableFlags(normalized, defs)).toBe(1);
  });
});
