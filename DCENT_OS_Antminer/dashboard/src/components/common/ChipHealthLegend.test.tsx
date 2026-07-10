/** @vitest-environment jsdom */

import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import {
  CHIP_HEALTH_COLORS,
  ChipHealthLegend,
  chipHealthToneFromAutotuner,
  chipHealthToneFromDiagnostics,
  formatHealthPercent,
} from './ChipHealthLegend';

describe('ChipHealthLegend', () => {
  it('keeps no-data health visually distinct from healthy', () => {
    expect(CHIP_HEALTH_COLORS['no-data']).not.toBe(CHIP_HEALTH_COLORS.healthy);
    expect(chipHealthToneFromDiagnostics({
      present: false,
      color: 'Gray',
      grade: 'X',
      healthScore: 0,
    })).toBe('no-data');
  });

  it('maps explicit failing and degraded states without treating scores as fabricated health', () => {
    expect(chipHealthToneFromDiagnostics({
      present: true,
      color: 'Red',
      grade: 'A',
      healthScore: 0.98,
    })).toBe('failing');
    expect(chipHealthToneFromAutotuner({
      status: 'warning',
      healthScore: 96,
    })).toBe('degraded');
    expect(chipHealthToneFromAutotuner({
      status: 'failed',
      healthScore: 96,
    })).toBe('failing');
  });

  it('formats both diagnostic 0-1 scores and autotuner 0-100 scores', () => {
    expect(formatHealthPercent(0.96)).toBe('96%');
    expect(formatHealthPercent(96)).toBe('96%');
    expect(formatHealthPercent(null)).toBe('n/a');
  });

  it('renders the health source label', () => {
    render(<ChipHealthLegend source="autotuner" />);
    expect(screen.getByTestId('chip-health-legend').getAttribute('data-health-source')).toBe('autotuner');
    expect(screen.getByText('from autotuner grading')).toBeTruthy();
  });
});
