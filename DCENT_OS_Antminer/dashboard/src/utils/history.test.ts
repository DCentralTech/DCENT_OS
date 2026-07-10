import { describe, it, expect } from 'vitest';
import { isBootZeroSample, dropBootZeroHistory } from './history';
import type { HistoryPoint } from '../api/types';

// P3-35(a): the daemon records a boot placeholder row (hashrate 0 + temp 0,
// pool still "Connecting") before mining starts. It is not a real sample and
// drags chart baselines/averages to the floor, so the leading boot-zero run is
// trimmed by every chart/history consumer (via api.normalizeHistory).

function pt(over: Partial<HistoryPoint>): HistoryPoint {
  return {
    timestamp: 0,
    hashrate_ghs: 0,
    temp_c: 0,
    power_watts: 0,
    fan_rpm: 0,
    ...over,
  };
}

describe('isBootZeroSample', () => {
  it('flags a sample with both hashrate and temp at zero', () => {
    expect(isBootZeroSample(pt({ hashrate_ghs: 0, temp_c: 0 }))).toBe(true);
  });

  it('does NOT flag a powered/idle board reporting a die temp', () => {
    expect(isBootZeroSample(pt({ hashrate_ghs: 0, temp_c: 42 }))).toBe(false);
  });

  it('does NOT flag a mining board (non-zero hashrate)', () => {
    expect(isBootZeroSample(pt({ hashrate_ghs: 12000, temp_c: 0 }))).toBe(false);
  });

  it('returns false for null/undefined', () => {
    expect(isBootZeroSample(null)).toBe(false);
    expect(isBootZeroSample(undefined)).toBe(false);
  });
});

describe('dropBootZeroHistory', () => {
  it('drops a single leading boot-zero row', () => {
    const out = dropBootZeroHistory([
      pt({ timestamp: 1, hashrate_ghs: 0, temp_c: 0 }),
      pt({ timestamp: 2, hashrate_ghs: 94800, temp_c: 62 }),
      pt({ timestamp: 3, hashrate_ghs: 95200, temp_c: 63 }),
    ]);
    expect(out.map(p => p.timestamp)).toEqual([2, 3]);
  });

  it('drops a multi-row leading boot-zero prefix only', () => {
    const out = dropBootZeroHistory([
      pt({ timestamp: 1, hashrate_ghs: 0, temp_c: 0 }),
      pt({ timestamp: 2, hashrate_ghs: 0, temp_c: 0 }),
      pt({ timestamp: 3, hashrate_ghs: 95000, temp_c: 62 }),
    ]);
    expect(out.map(p => p.timestamp)).toEqual([3]);
  });

  it('PRESERVES genuine mid-stream zeros (a stall is real data)', () => {
    const out = dropBootZeroHistory([
      pt({ timestamp: 1, hashrate_ghs: 95000, temp_c: 62 }),
      pt({ timestamp: 2, hashrate_ghs: 0, temp_c: 0 }), // mid-stream stall — keep
      pt({ timestamp: 3, hashrate_ghs: 95000, temp_c: 62 }),
    ]);
    expect(out.map(p => p.timestamp)).toEqual([1, 2, 3]);
  });

  it('returns the same reference when there is no boot-zero prefix', () => {
    const input = [pt({ timestamp: 1, hashrate_ghs: 95000, temp_c: 62 })];
    expect(dropBootZeroHistory(input)).toBe(input);
  });

  it('handles an all-boot-zero series (miner never started) → empty', () => {
    const out = dropBootZeroHistory([
      pt({ timestamp: 1 }),
      pt({ timestamp: 2 }),
    ]);
    expect(out).toEqual([]);
  });

  it('handles an empty array', () => {
    expect(dropBootZeroHistory([])).toEqual([]);
  });
});
