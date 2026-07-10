/** @vitest-environment jsdom */

import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import { describe, expect, it } from 'vitest';

import { chipLivenessPulseOrdinals } from './ChipHeatMap';
import { chipHealthToneFromDiagnostics } from '../common/ChipHealthLegend';

const SOURCE = readFileSync(join(process.cwd(), 'src/components/standard/ChipHeatMap.tsx'), 'utf8');

describe('ChipHeatMap liveness honesty', () => {
  it('derives chip liveness pulse cells deterministically from present-chip count', () => {
    expect(chipLivenessPulseOrdinals(0, 123)).toEqual([]);
    expect(chipLivenessPulseOrdinals(1, 99)).toEqual([0]);
    expect(chipLivenessPulseOrdinals(96, 0)).toEqual([0, 13]);
    expect(chipLivenessPulseOrdinals(96, 97)).toEqual([1, 14]);
    expect(chipLivenessPulseOrdinals(192, -1)).toEqual([191, 12, 25, 38]);
  });

  it('keeps the chip grid free of random live-looking chip activity', () => {
    expect(SOURCE).not.toMatch(/Math\.random/);
    expect(SOURCE).not.toMatch(/chosen at random|random cell/i);
  });

  it('renders absent health as no-data instead of healthy', () => {
    expect(chipHealthToneFromDiagnostics({
      present: false,
      color: 'Gray',
      grade: 'X',
      healthScore: 0,
    })).toBe('no-data');
    expect(chipHealthToneFromDiagnostics({
      present: false,
      color: 'Gray',
      grade: 'A',
      healthScore: 1,
    })).not.toBe('healthy');
  });
});
