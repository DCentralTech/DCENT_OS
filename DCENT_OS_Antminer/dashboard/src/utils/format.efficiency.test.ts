import { describe, it, expect } from 'vitest';
import { formatEfficiency, isUnsupportedMetric, unsupportedMetricList } from './format';

// Omega P3-3: a catastrophic efficiency (>10,000 J/TH) means hashrate has
// collapsed on a degraded/near-dead unit. It must ALARM with the honest bound,
// not blank to "—" (which would silently hide a broken miner). A non-positive /
// non-finite value still renders "—" because there is no real hashrate to
// divide by yet — never fabricate a number to look healthy (truth contract).
describe('formatEfficiency (P3-3 degraded-unit alarm)', () => {
  it('formats a real operating efficiency to one decimal', () => {
    expect(formatEfficiency(27.6)).toBe('27.6 J/TH');
    expect(formatEfficiency(100)).toBe('100.0 J/TH');
    expect(formatEfficiency(10000)).toBe('10000.0 J/TH'); // exactly at the bound is still real
  });

  it('ALARMS instead of hiding when efficiency is catastrophic (>10,000 J/TH)', () => {
    expect(formatEfficiency(10000.1)).toBe('>10,000 J/TH (degraded)');
    expect(formatEfficiency(50000)).toBe('>10,000 J/TH (degraded)');
    expect(formatEfficiency(Number.POSITIVE_INFINITY)).toBe('—'); // not a real number → placeholder, not alarm
  });

  it('renders an honest placeholder when there is no real input', () => {
    expect(formatEfficiency(0)).toBe('—');
    expect(formatEfficiency(-5)).toBe('—');
    expect(formatEfficiency(Number.NaN)).toBe('—');
  });
});

// Omega P3-8: AxeOS/pyasic-compat fields the daemon emits as 0 are flagged in
// `unsupported_metrics` (REST) / `_DCENTUnsupported` (CGMiner). The UI reads the
// flag so a 0 is shown as n/a, never as a measurement.
describe('unsupported-metric flag (P3-8)', () => {
  it('normalizes a flag list and ignores junk entries', () => {
    expect(unsupportedMetricList(['vrTemp', 'bestDiff'])).toEqual(['vrTemp', 'bestDiff']);
    expect(unsupportedMetricList(['vrTemp', '', null as unknown as string, 7 as unknown as string]))
      .toEqual(['vrTemp']);
  });

  it('returns an empty list for missing / non-array flags', () => {
    expect(unsupportedMetricList(undefined)).toEqual([]);
    expect(unsupportedMetricList(null)).toEqual([]);
  });

  it('detects whether a specific metric is flagged unsupported', () => {
    const flags = ['bestDiff', 'vrTemp', 'showNewBlock'];
    expect(isUnsupportedMetric('vrTemp', flags)).toBe(true);
    expect(isUnsupportedMetric('hashrate', flags)).toBe(false);
    expect(isUnsupportedMetric('vrTemp', undefined)).toBe(false);
  });
});
