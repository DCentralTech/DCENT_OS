import { describe, it, expect } from 'vitest';
import { classifyThermalSupervisor } from './thermalSupervisor';
import type { ThermalSupervisorSnapshot, BoardStateSnapshot } from '../api/types';

// Omega P3-37: the dashboard must surface the thermal supervisor's disabled /
// empty state honestly, and carry the load-bearing die-fallback caveat (a >=70
// °C "dangerous" board-panic alert can never fire when the only temperature
// source is the SoC-die fallback).

function board(overrides: Partial<BoardStateSnapshot> = {}): BoardStateSnapshot {
  return {
    chain_id: 0,
    recovery_attempts: 0,
    dropped_pcb_sensors: 0,
    dropped_chip_sensors: 0,
    chip_imbalance_c: null,
    chip_imbalance_flagged: false,
    ...overrides,
  };
}

function snap(overrides: Partial<ThermalSupervisorSnapshot> = {}): ThermalSupervisorSnapshot {
  return {
    enabled: true,
    uptime_secs: 0,
    secs_since_last_step: 0,
    board_states: [],
    fan_max_pwm: 30,
    chip_imbalance_threshold_c: 15,
    worst_chip_imbalance_c: null,
    hydro_configured: false,
    ...overrides,
  };
}

describe('classifyThermalSupervisor', () => {
  it('maps an unreachable snapshot to unavailable with no caveat or imbalance', () => {
    const v = classifyThermalSupervisor(null);
    expect(v.availability).toBe('unavailable');
    expect(v.dieFallbackCaveat).toBe(false);
    expect(v.imbalance).toBeNull();
  });

  it('renders the disabled (default) state honestly — never as active protection', () => {
    const v = classifyThermalSupervisor(snap({ enabled: false, board_states: [] }));
    expect(v.availability).toBe('disabled');
    // No board temps supervised → only the die fallback → >=70 alert can't fire.
    expect(v.dieFallbackCaveat).toBe(true);
    // The imbalance diagnostic is suppressed (not "no multi-sensor data").
    expect(v.imbalance).toBeNull();
  });

  it('flags the die-fallback caveat when enabled but no per-board sensors are read', () => {
    const v = classifyThermalSupervisor(snap({ enabled: true, board_states: [] }));
    expect(v.availability).toBe('active');
    expect(v.dieFallbackCaveat).toBe(true);
    expect(v.imbalance).not.toBeNull();
    expect(v.imbalance?.worst).toBeNull();
  });

  it('does NOT show the die-fallback caveat once real board sensors are present', () => {
    const v = classifyThermalSupervisor(
      snap({ enabled: true, board_states: [board({ chip_imbalance_c: 3.2 })] }),
    );
    expect(v.availability).toBe('active');
    expect(v.dieFallbackCaveat).toBe(false);
    expect(v.imbalance?.flagged).toBe(false);
  });

  it('flags imbalance when a board flag is set', () => {
    const v = classifyThermalSupervisor(
      snap({ enabled: true, board_states: [board({ chip_imbalance_flagged: true })] }),
    );
    expect(v.imbalance?.flagged).toBe(true);
  });

  it('flags imbalance when the worst aggregate meets the threshold', () => {
    const v = classifyThermalSupervisor(
      snap({
        enabled: true,
        board_states: [board()],
        worst_chip_imbalance_c: 16,
        chip_imbalance_threshold_c: 15,
      }),
    );
    expect(v.imbalance?.flagged).toBe(true);
  });
});
