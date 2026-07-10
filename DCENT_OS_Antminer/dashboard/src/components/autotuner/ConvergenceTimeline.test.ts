// @vitest-environment jsdom

import { describe, expect, it } from 'vitest';

import {
  buildConvergenceRows,
  convergenceProgressText,
  latestTelemetryRun,
} from './ConvergenceTimeline';
import type { AutotunerStatusResponse, AutotunerTelemetryResponse } from '../../api/types';

const statusBase: AutotunerStatusResponse = {
  enabled: true,
  live_runtime: true,
  stale: false,
  age_s: 1,
  source: 'runtime',
  state: 'characterizing',
  phase: 'characterizing',
  percent_complete: 0,
  completed_chips: 0,
  active_chips: 2,
  total_chips: 2,
  target_chains: 1,
  tuned_chains: 0,
  failed_chains: 0,
  tuned_chain_ids: [],
  failed_chain_ids: [],
  last_update_s: 1_779_999_600,
  message: 'characterizing',
};

const telemetry: AutotunerTelemetryResponse = {
  live_runtime: true,
  recording: true,
  last_update_s: 1_779_999_600,
  message: 'recording',
  runs: [
    {
      started_at: 1_779_990_000,
      duration_s: 120,
      completed: false,
      samples: [
        {
          elapsed_s: 10,
          chain_id: 6,
          board_temp_c: 55.2,
          tuner_state: 'characterizing',
          difficulty: 512,
          chips: [
            { chip_index: 0, nonces: 100, errors: 0, freq_mhz: 600, decision: 'hold' },
            { chip_index: 1, nonces: 80, errors: 2, freq_mhz: 620, decision: 'lower_freq' },
          ],
        },
      ],
    },
  ],
};

describe('ConvergenceTimeline derivations', () => {
  it('flattens real telemetry samples without inventing missing columns', () => {
    const rows = buildConvergenceRows(latestTelemetryRun(telemetry));
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({
      step: 1,
      chainId: 6,
      chipCount: 2,
      avgFreqMhz: 610,
      totalNonces: 180,
      totalErrors: 2,
      boardTempC: 55.2,
      state: 'characterizing',
      difficulty: 512,
      decisions: ['hold', 'lower_freq'],
    });
  });

  it('does not print an ETA unless the daemon supplies remaining seconds', () => {
    const rows = buildConvergenceRows(latestTelemetryRun(telemetry));
    expect(convergenceProgressText(statusBase, rows)).toBe('Step 1, target not yet reached.');
    expect(convergenceProgressText({
      ...statusBase,
      estimated_remaining_s: 95,
    }, rows)).toBe('Estimated remaining 1m 35s from daemon status.');
  });
});
