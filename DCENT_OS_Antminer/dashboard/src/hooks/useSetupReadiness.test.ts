// @vitest-environment jsdom

import { describe, expect, it } from 'vitest';
import type { PowerCalibrationResponse, SetupStatusResponse } from '../api/types';
import {
  deriveSetupReadinessTaskSeeds,
  selectVisibleReadinessTaskSeeds,
} from './useSetupReadiness';

const calibrationOff = {
  enabled: false,
  calibrated: false,
} as PowerCalibrationResponse;

function setupStatus(overrides: Partial<SetupStatusResponse> = {}): SetupStatusResponse {
  return {
    needs_setup: false,
    device_ready: true,
    mining_ready: false,
    safety_opt_out: true,
    safety_decision_made: true,
    steps: ['pool', 'complete'],
    progress: {
      safety: false,
      circuit: false,
      password: true,
      mode: true,
      pool: false,
      complete: true,
    },
    current: {
      hostname: '',
      mode: 'standard',
      power_source: '',
      pool: { url: '', worker: '' },
    },
    ...overrides,
  };
}

describe('setup readiness derivation', () => {
  it('surfaces Quick Start deferrals without re-adding opted-out safety', () => {
    const tasks = selectVisibleReadinessTaskSeeds({
      setupStatus: setupStatus(),
      minerName: 'My Miner',
      calibration: calibrationOff,
    });

    expect(tasks.map(task => task.id)).toEqual([
      'pool',
      'power_source',
      'miner_name',
      'power_calibration',
    ]);
    expect(tasks).toHaveLength(4);
  });

  it('removes dismissed tasks before applying the four-item cap', () => {
    const tasks = selectVisibleReadinessTaskSeeds({
      setupStatus: setupStatus(),
      minerName: 'My Miner',
      calibration: calibrationOff,
    }, ['miner_name']);

    expect(tasks.map(task => task.id)).toEqual([
      'pool',
      'power_source',
      'power_calibration',
    ]);
  });

  it('falls back to mining verification only when setup tasks are complete', () => {
    const tasks = deriveSetupReadinessTaskSeeds({
      setupStatus: setupStatus({
        current: {
          hostname: 'rig-one',
          mode: 'standard',
          power_source: 'grid',
          pool: { url: 'stratum+tcp://pool.example:3333', worker: 'rig-one' },
        },
        progress: {
          safety: true,
          circuit: true,
          password: true,
          mode: true,
          pool: true,
          complete: true,
        },
      }),
      minerName: 'Rig One',
      calibration: { ...calibrationOff, enabled: true, calibrated: true },
    });

    expect(tasks.map(task => task.id)).toEqual(['verify']);
  });
});
