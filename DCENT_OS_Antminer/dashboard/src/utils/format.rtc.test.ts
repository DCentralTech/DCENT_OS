import { describe, it, expect } from 'vitest';
import { isRtcSyncedMs, RTC_SANE_EPOCH_MS } from './format';

// P2-1 / D-18: a timestamp from before ~2020 means the miner had no RTC and the
// clock had not been stepped by SNTP when the value was recorded. The guard must
// reject those so callers render "—"/"since boot" instead of a 1970 wall-clock
// date. `new Date(ts)` is only allowed once isRtcSyncedMs(ts) is true.

describe('isRtcSyncedMs (pre-2020 RTC guard)', () => {
  it('rejects the 1970 boot epoch and early-boot times (no RTC)', () => {
    expect(isRtcSyncedMs(0)).toBe(false);
    expect(isRtcSyncedMs(60_000)).toBe(false); // ~1 min after boot, unsynced
    expect(isRtcSyncedMs(946_684_800_000)).toBe(false); // 2000-01-01
    expect(isRtcSyncedMs(RTC_SANE_EPOCH_MS - 1)).toBe(false); // just before 2020
  });

  it('accepts the 2020 threshold and realistic recent times', () => {
    expect(isRtcSyncedMs(RTC_SANE_EPOCH_MS)).toBe(true); // 2020-01-01 exactly
    expect(isRtcSyncedMs(1_780_000_000_000)).toBe(true); // ~2026
    expect(isRtcSyncedMs(Date.now())).toBe(true);
  });

  it('rejects null / undefined / NaN / non-finite inputs', () => {
    expect(isRtcSyncedMs(null)).toBe(false);
    expect(isRtcSyncedMs(undefined)).toBe(false);
    expect(isRtcSyncedMs(Number.NaN)).toBe(false);
    expect(isRtcSyncedMs(Number.POSITIVE_INFINITY)).toBe(false);
  });

  it('pins the threshold to 2020-01-01T00:00:00Z so it stays a documented constant', () => {
    expect(RTC_SANE_EPOCH_MS).toBe(Date.parse('2020-01-01T00:00:00Z'));
  });

  // Renders the way the guarded call sites do: only a synced value becomes a
  // wall-clock string; everything pre-2020 falls back to a dash. This mirrors
  // EarningsChart.fmtTs and EvidencePage hero/formatTimeMs.
  function renderTs(ts: number | null | undefined, fallback = '—'): string {
    return isRtcSyncedMs(ts) ? new Date(ts as number).toISOString() : fallback;
  }

  it('never renders a 1970-era date string through the guarded path', () => {
    expect(renderTs(0)).toBe('—');
    expect(renderTs(120_000)).toBe('—');
    expect(renderTs(null)).toBe('—');
    expect(renderTs(1_780_000_000_000)).toBe('2026-05-28T20:26:40.000Z');
  });
});
