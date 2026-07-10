import { describe, expect, it } from 'vitest';

import {
  getLiveDisplayWallWatts,
  getLiveHistoryPointWallWatts,
  getLivePowerEfficiencyJth,
  getLiveWallWatts,
  getPowerTargetingLabel,
  getPowerTelemetryLabel,
  hasLiveWallPower,
} from './power';

describe('power telemetry helpers', () => {
  it('does not treat static fallback watts as live wall power', () => {
    const power = {
      wall_watts: 1234,
      watts: 1100,
      source: 'static_model_fallback',
      live_power_available: false,
      modeled: true,
    };

    expect(hasLiveWallPower(power)).toBe(false);
    expect(getLiveWallWatts(power)).toBe(0);
    expect(getPowerTelemetryLabel(power)).toBe('Modeled fallback estimate');
  });

  it('labels unavailable live frames distinctly from modeled fallbacks', () => {
    const power = {
      wall_watts: 0,
      watts: 0,
      source: 'unavailable',
      source_detail: 'live_power_unavailable',
      live_power_available: false,
      modeled: false,
    };

    expect(hasLiveWallPower(power)).toBe(false);
    expect(getLiveWallWatts(power)).toBe(0);
    expect(getPowerTelemetryLabel(power)).toBe('Power telemetry unavailable');
  });

  it('accepts positive PMBus and ADC readings as live wall power', () => {
    expect(getLiveWallWatts({
      wall_watts: 1234,
      source: 'pmbus',
      source_detail: 'pmbus_measured',
      live_power_available: true,
    })).toBe(1234);
    expect(getPowerTelemetryLabel({
      wall_watts: 960,
      source: 'adc',
      source_detail: 'adc_measured',
      live_power_available: true,
    })).toBe('ADC measured power');
  });

  it('rejects provenance-free or estimated watts as live wall power', () => {
    expect(getLiveWallWatts({ watts: 900 })).toBe(0);
    expect(getLiveWallWatts({ wall_watts: 900 })).toBe(0);
    expect(getLiveWallWatts({ wall_watts: 900, source: 'estimated' })).toBe(0);
  });

  it('accepts legacy measured PMBus or ADC source names without a live flag', () => {
    expect(getLiveWallWatts({ wall_watts: 900, source: 'pmbus' })).toBe(900);
    expect(getLiveWallWatts({ wall_watts: 800, source: 'adc' })).toBe(800);
  });

  it('keeps heater display fallback watts out of live wall-power claims', () => {
    expect(getLiveDisplayWallWatts({
      wall_watts: 1234,
      power_watts: 1100,
      source: 'static_model_fallback',
      live_power_available: false,
      power_modeled: true,
    }, {
      wall_watts: 0,
      watts: 1100,
      source: 'static_model_fallback',
      live_power_available: false,
      modeled: true,
    })).toBe(0);

    expect(getLiveDisplayWallWatts({
      wall_watts: 1234,
      power_watts: 1100,
    }, {
      wall_watts: 900,
      source: 'adc',
      live_power_available: true,
    })).toBe(900);
  });

  it('prefers live heater wall power and otherwise falls back to live stats power', () => {
    expect(getLiveDisplayWallWatts({
      wall_watts: 1200,
      power_watts: 1100,
      source: 'pmbus',
      live_power_available: true,
    }, {
      wall_watts: 900,
      source: 'adc',
      live_power_available: true,
    })).toBe(1200);

    expect(getLiveDisplayWallWatts({
      power_watts: 1100,
      source: 'static_model_fallback',
      live_power_available: false,
      power_modeled: true,
    }, {
      wall_watts: 900,
      source: 'adc',
      live_power_available: true,
    })).toBe(900);
  });

  it('accepts only live-provenance history point watts', () => {
    expect(getLiveHistoryPointWallWatts({
      power_watts: 1320,
      power_source: 'pmbus',
      power_source_detail: 'pmbus_measured',
      live_power_available: true,
      power_modeled: false,
    })).toBe(1320);

    expect(getLiveHistoryPointWallWatts({
      power_watts: 1350,
      power_source: 'legacy_unprovenanced',
      power_source_detail: 'legacy_history_without_provenance',
      live_power_available: false,
      power_modeled: true,
    })).toBe(0);

    expect(getLiveHistoryPointWallWatts({
      power_watts: 1180,
      power_source: 'unavailable',
      power_source_detail: 'live_power_unavailable',
      live_power_available: false,
      power_modeled: false,
    })).toBe(0);
  });

  it('accepts legacy efficiency only when its power object is live-provenance', () => {
    expect(getLivePowerEfficiencyJth({
      wall_watts: 1320,
      efficiency_jth: 33.5,
      source: 'pmbus',
      source_detail: 'pmbus_measured',
      live_power_available: true,
    })).toBe(33.5);

    expect(getLivePowerEfficiencyJth({
      wall_watts: 1320,
      efficiency_jth: 33.5,
      source: 'static_model_fallback',
      source_detail: 'static_power_fallback_from_miner_state',
      live_power_available: false,
      modeled: true,
    })).toBe(0);

    expect(getLivePowerEfficiencyJth({
      wall_watts: 1320,
      efficiency_jth: 33.5,
    })).toBe(0);
  });

  it('renders power-targeting delta only from measured current wall power', () => {
    expect(getPowerTargetingLabel({
      targeting: {
        active: true,
        source: 'autotuner',
        mode: 'power',
        target_watts: 1200,
        current_wall_watts: 1260,
        current_wall_watts_measured: true,
        current_wall_watts_source_detail: 'pmbus_measured',
        delta_watts: 60,
        comparison: 'over',
      },
    })).toBe('Power mode: 1,200 W target, 60 W over');

    expect(getPowerTargetingLabel({
      targeting: {
        active: true,
        source: 'autotuner',
        mode: 'power',
        target_watts: 1200,
        current_wall_watts: 0,
        current_wall_watts_measured: false,
        current_wall_watts_source_detail: null,
        delta_watts: 60,
        comparison: 'over',
      },
    })).toBe('Power mode: 1,200 W target active');
  });
});
