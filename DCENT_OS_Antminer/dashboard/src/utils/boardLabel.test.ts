import { describe, it, expect } from 'vitest';
import { boardConnector, boardName, boardDescriptor, boardLabel } from './boardLabel';

// Omega P3-5: one canonical "Board N (Jx · chainN)" label, replacing the
// J6/J7/J8 vs "Chain 6/7/8" vs CH0/1/2 mix.

describe('boardLabel', () => {
  it('maps the S9 chains 6/7/8 to canonical Board N (Jx · chainN)', () => {
    expect(boardLabel(0, 6)).toBe('Board 1 (J6 · chain6)');
    expect(boardLabel(1, 7)).toBe('Board 2 (J7 · chain7)');
    expect(boardLabel(2, 8)).toBe('Board 3 (J8 · chain8)');
  });

  it('keys the connector off board POSITION, not the chain id', () => {
    // am2 / .25-class: two populated boards on chain ids 0 and 2 still read
    // J6/J7 by physical slot (the connector is silk-screened, not chain-id'd).
    expect(boardLabel(0, 0)).toBe('Board 1 (J6 · chain0)');
    expect(boardLabel(1, 2)).toBe('Board 2 (J7 · chain2)');
  });

  it('falls back to J<chainId> beyond the known connector silk labels', () => {
    expect(boardConnector(9, 42)).toBe('J42');
    expect(boardLabel(9, 42)).toBe('Board 10 (J42 · chain42)');
  });

  it('exposes the name and descriptor parts for split rendering', () => {
    expect(boardName(0)).toBe('Board 1');
    expect(boardDescriptor(0, 6)).toBe('J6 · chain6');
  });
});
