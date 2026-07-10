// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flashAlert, resetTitleTicker, updateTitleTicker } from './titleTicker';

beforeEach(() => {
  vi.useFakeTimers();
  resetTitleTicker();
});

afterEach(() => {
  resetTitleTicker();
  vi.useRealTimers();
});

describe('titleTicker', () => {
  it('renders hashrate only while telemetry is recent and ticker is enabled', () => {
    updateTitleTicker({
      hashrateGhs: 1050,
      hasRecentTelemetry: true,
      enabled: true,
      now: 10_000,
    });
    expect(document.title).toBe('DCENT_OS - 1.05 TH/s');

    updateTitleTicker({
      hashrateGhs: 2050,
      hasRecentTelemetry: true,
      enabled: true,
      now: 15_000,
    });
    expect(document.title).toBe('DCENT_OS - 1.05 TH/s');

    updateTitleTicker({
      hashrateGhs: 2050,
      hasRecentTelemetry: true,
      enabled: true,
      now: 21_000,
    });
    expect(document.title).toBe('DCENT_OS - 2.05 TH/s');

    updateTitleTicker({
      hashrateGhs: 2050,
      hasRecentTelemetry: false,
      enabled: true,
      now: 22_000,
    });
    expect(document.title).toBe('DCENT_OS');
  });

  it('lets critical alerts override telemetry and then restores the base title', () => {
    updateTitleTicker({
      hashrateGhs: 1050,
      hasRecentTelemetry: true,
      enabled: true,
      now: 10_000,
    });

    flashAlert('Thermal trip', 1000);
    expect(document.title).toBe('[!] Thermal trip - DCENT_OS');

    vi.advanceTimersByTime(1000);
    expect(document.title).toBe('DCENT_OS - 1.05 TH/s');
  });
});
