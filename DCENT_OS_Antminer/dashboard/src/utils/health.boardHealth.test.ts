import { describe, it, expect } from 'vitest';
import {
  classifyBoardHealth,
  summarizeBoardHealth,
  getDashboardHealth,
  BOARD_FAULT_ERROR_THRESHOLD,
} from './health';
import type { ChainState, StatusResponse } from '../api/types';

// P0-3 (C-3/D-3): a 0-GH/s dead board the daemon still reports as
// `status:"Active"` must be flagged degraded/fault — board health is derived
// from hashrate-while-powered, never from the status string.

function chain(overrides: Partial<ChainState>): ChainState {
  return {
    id: 0,
    chips: 63,
    frequency_mhz: 650,
    voltage_mv: 0,
    temp_c: 50,
    hashrate_ghs: 4000,
    errors: 0,
    status: 'Active',
    ...overrides,
  };
}

describe('classifyBoardHealth', () => {
  it('flags a powered, clocked board producing ~zero hashrate as degraded while the unit is mining', () => {
    const board = chain({ hashrate_ghs: 0, errors: 5, status: 'Active' });
    expect(classifyBoardHealth(board, true)).toBe('degraded');
  });

  it('promotes a non-hashing board to fault when hardware errors are high', () => {
    const board = chain({ hashrate_ghs: 0, errors: BOARD_FAULT_ERROR_THRESHOLD, status: 'Active' });
    expect(classifyBoardHealth(board, true)).toBe('fault');
  });

  it('treats an actively hashing board as healthy regardless of the status string', () => {
    expect(classifyBoardHealth(chain({ hashrate_ghs: 4000, status: 'whatever' }), true)).toBe('healthy');
  });

  it('does NOT cry fault on a powered idle board when the whole unit is not mining', () => {
    const board = chain({ hashrate_ghs: 0, errors: 0, status: 'Active' });
    expect(classifyBoardHealth(board, false)).toBe('idle');
  });

  it('treats an unpowered board (no chips) as idle, not fault', () => {
    expect(classifyBoardHealth(chain({ chips: 0, hashrate_ghs: 0 }), true)).toBe('idle');
  });

  it('honors an explicit dead/fault status even with capitalization', () => {
    expect(classifyBoardHealth(chain({ hashrate_ghs: 0, status: 'DEAD' }), false)).toBe('fault');
  });
});

describe('summarizeBoardHealth', () => {
  it('counts one not-hashing board among two healthy ones', () => {
    const chains = [
      chain({ id: 7, hashrate_ghs: 4000 }),
      chain({ id: 8, hashrate_ghs: 3500, errors: 5 }),
      chain({ id: 6, hashrate_ghs: 0, errors: 52, status: 'Active' }),
    ];
    const summary = summarizeBoardHealth(chains, true);
    expect(summary.total).toBe(3);
    expect(summary.notHashing).toBe(1);
    expect(summary.faulted).toBe(1);
    expect(summary.degraded).toBe(0);
    expect(summary.worst).toBe('fault');
    expect(summary.perBoard).toEqual(['healthy', 'healthy', 'fault']);
  });
});

describe('getDashboardHealth board-not-hashing issue', () => {
  function statusWith(chains: ChainState[], hashrateGhs: number): StatusResponse {
    return {
      hashrate_ghs: hashrateGhs,
      chains,
      fans: { pwm: 10, rpm: 1200 },
      pool: { status: 'connected' },
    } as unknown as StatusResponse;
  }

  it('emits a critical board-not-hashing issue for a dead "Active" board while mining', () => {
    const chains = [
      chain({ id: 7, hashrate_ghs: 4000 }),
      chain({ id: 8, hashrate_ghs: 3500, errors: 5 }),
      chain({ id: 6, hashrate_ghs: 0, errors: 52, status: 'Active' }),
    ];
    const health = getDashboardHealth({
      status: statusWith(chains, 7500),
      wsConnected: true,
      lastUpdate: Date.now(),
      setupStatus: null,
    });

    const issue = health.issues.find(i => i.key === 'board-not-hashing');
    expect(issue).toBeDefined();
    expect(issue?.level).toBe('critical');
    expect(issue?.message).toContain('1 of 3 boards not hashing');

    // The miner is still hashing overall — the isMining-driven chip is untouched
    // (P0-7 owns that selector; this item leaves it alone).
    expect(health.minerChip.label).toBe('Mining');
    expect(health.boardHealth.notHashing).toBe(1);
  });

  it('does not emit the issue when the whole unit is not mining', () => {
    const chains = [
      chain({ id: 7, hashrate_ghs: 0, status: 'Active' }),
      chain({ id: 8, hashrate_ghs: 0, status: 'Active' }),
    ];
    const health = getDashboardHealth({
      status: statusWith(chains, 0),
      wsConnected: true,
      lastUpdate: Date.now(),
      setupStatus: null,
    });
    expect(health.issues.find(i => i.key === 'board-not-hashing')).toBeUndefined();
    expect(health.boardHealth.notHashing).toBe(0);
  });

  it('treats missing chain telemetry as unavailable instead of crashing', () => {
    const partialStatus = {
      hashrate_ghs: 7500,
      fans: { pwm: 10, rpm: 1200 },
      pool: { status: 'connected' },
    } as unknown as StatusResponse;

    const health = getDashboardHealth({
      status: partialStatus,
      wsConnected: true,
      lastUpdate: Date.now(),
      setupStatus: null,
    });

    expect(health.minerChip.label).toBe('Mining');
    expect(health.boardHealth.total).toBe(0);
    expect(health.issues.some(issue => issue.key.startsWith('hot-chain-'))).toBe(false);
    expect(health.issues.some(issue => issue.key.startsWith('chain-missing-'))).toBe(false);
  });
});
