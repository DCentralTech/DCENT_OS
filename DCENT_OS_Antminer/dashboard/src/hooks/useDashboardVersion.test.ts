import { describe, expect, it } from 'vitest';
import {
  normalizeDashboardSha256,
  shouldRecheckDashboardVersion,
  shouldShowDashboardVersionMismatch,
} from './useDashboardVersion';

const HASH_A = 'a'.repeat(64);
const HASH_B = 'b'.repeat(64);

describe('dashboard version comparison', () => {
  it('normalizes valid SHA-256 strings', () => {
    expect(normalizeDashboardSha256(` ${HASH_A.toUpperCase()} `)).toBe(HASH_A);
  });

  it('rejects missing or malformed hashes', () => {
    expect(normalizeDashboardSha256(null)).toBeNull();
    expect(normalizeDashboardSha256('missing')).toBeNull();
    expect(normalizeDashboardSha256('a'.repeat(63))).toBeNull();
  });

  it('reports mismatch only when both hashes are valid and different', () => {
    expect(shouldShowDashboardVersionMismatch(HASH_A, { sha256: HASH_A })).toBe(false);
    expect(shouldShowDashboardVersionMismatch(HASH_A, { sha256: HASH_B })).toBe(true);
    expect(shouldShowDashboardVersionMismatch(null, { sha256: HASH_B })).toBe(false);
    expect(shouldShowDashboardVersionMismatch(HASH_A, { sha256: null })).toBe(false);
  });

  it('rechecks on first run and after the one-hour focus window', () => {
    expect(shouldRecheckDashboardVersion(0, 1)).toBe(true);
    expect(shouldRecheckDashboardVersion(1_000, 1_000 + 59 * 60 * 1000)).toBe(false);
    expect(shouldRecheckDashboardVersion(1_000, 1_000 + 61 * 60 * 1000)).toBe(true);
  });
});
