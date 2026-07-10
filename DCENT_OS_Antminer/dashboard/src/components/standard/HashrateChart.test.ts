// @vitest-environment jsdom

import { describe, expect, it } from 'vitest';

import type { RollingMetricsResponse } from '../../api/types';
import { rollingHashratePoint } from './HashrateChart';

function metrics(sampleCount: number, avgHashrateThs: number): RollingMetricsResponse {
  const bucket = {
    window_s: 60,
    sample_count: sampleCount,
    avg_hashrate_ths: avgHashrateThs,
    avg_wall_watts: 0,
    wall_power_sample_count: 0,
    wall_power_measured_sample_count: 0,
    wall_power_modeled_sample_count: 0,
    wall_power_unavailable_sample_count: sampleCount,
    avg_max_chip_temp_c: 0,
    avg_error_rate: 0,
    avg_max_fan_pwm: 0,
    accepted_shares: 0,
    rejected_shares: 0,
  };
  return {
    now_ms: 1_800_000,
    total_samples: sampleCount,
    w5s: { ...bucket, window_s: 5 },
    w1m: bucket,
    w5m: { ...bucket, window_s: 300 },
  };
}

describe('HashrateChart rolling metrics source', () => {
  it('converts the daemon 1m TH/s average into chart GH/s points', () => {
    expect(rollingHashratePoint(metrics(12, 95.25))).toEqual({
      time: 1800,
      value: 95_250,
    });
  });

  it('does not create a rolling point without real samples', () => {
    expect(rollingHashratePoint(metrics(0, 100))).toBeNull();
    expect(rollingHashratePoint(metrics(4, -1))).toBeNull();
  });
});
