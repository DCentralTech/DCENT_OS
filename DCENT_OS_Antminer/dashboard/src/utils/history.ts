// History-series hygiene helpers (pure — unit-tested in history.test.ts).

import type { HistoryPoint } from '../api/types';

/**
 * P3-35(a): a "boot-zero" history sample.
 *
 * On a fresh daemon start the first recorded history row(s) are a boot
 * placeholder: `hashrate_ghs: 0` AND `temp_c: 0` (no telemetry yet — the pool
 * is still "Connecting"). A real powered board always reports a non-zero die
 * temperature, and a real mining board a non-zero hashrate, so a sample with
 * BOTH at zero is not a real data point. Requiring BOTH to be zero is the
 * conservative test: a genuine idle-but-powered board reports a die temp, and a
 * mid-stream stall keeps its last temp — neither is misclassified.
 */
export function isBootZeroSample(point: HistoryPoint | null | undefined): boolean {
  if (!point) return false;
  return (point.hashrate_ghs ?? 0) <= 0 && (point.temp_c ?? 0) <= 0;
}

/**
 * Trim the LEADING boot-zero run from a history series.
 *
 * Only the leading prefix is dropped — genuine mid-stream zeros (a stall, a
 * curtailment, a pool drop) are preserved so the chart stays honest. Without
 * this the chart baseline and average lines are dragged down to the floor by a
 * fabricated 0/0 first point. Returns the same array reference when there is
 * nothing to trim (cheap, allocation-free no-op).
 */
export function dropBootZeroHistory(history: HistoryPoint[]): HistoryPoint[] {
  if (!Array.isArray(history) || history.length === 0) return history;
  let start = 0;
  while (start < history.length && isBootZeroSample(history[start])) {
    start += 1;
  }
  return start === 0 ? history : history.slice(start);
}
