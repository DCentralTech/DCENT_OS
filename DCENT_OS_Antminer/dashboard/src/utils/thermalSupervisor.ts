// Thermal-supervisor honesty helpers (Omega P3-37).
//
// The thermal supervisor (`/api/thermal/supervisor`) is an OPTIONAL diagnostic
// + escalation layer that is OFF by default. The dashboard must surface that
// disabled/empty state honestly — never render it in a way that implies active
// thermal protection that isn't there. It must also carry the load-bearing
// die-fallback caveat: when no per-board PCB/chip sensors are read, the SoC-die
// fallback is the only temperature source, and the die runs far cooler than the
// chip junction, so the supervisor's >=70 °C "dangerous" board-panic escalation
// can never fire from die temperature alone.
//
// Pure logic only (no React) so it is unit-testable under the node-env vitest
// config, mirroring utils/health.ts and utils/format.ts.
import type { ThermalSupervisorSnapshot, BoardStateSnapshot } from '../api/types';

export type ThermalSupervisorAvailability = 'unavailable' | 'disabled' | 'active';

/** Diagnostic inter-chip imbalance summary — only meaningful while active. */
export interface ThermalImbalanceView {
  /** Worst inter-chip die-temp spread (°C), or null until >= 2 valid sensors. */
  worst: number | null;
  /** Diagnostic flag threshold (°C) in effect. */
  threshold: number;
  /** true when any board (or the worst aggregate) exceeded the threshold. */
  flagged: boolean;
  boards: BoardStateSnapshot[];
}

export interface ThermalSupervisorView {
  /** Honest top-line state for the status chip. */
  availability: ThermalSupervisorAvailability;
  /**
   * True when the supervisor is reading NO per-board PCB/chip temperature
   * sensors (`board_states` empty). In that regime the SoC-die fallback is the
   * only temperature source, so the supervisor's >=70 °C "dangerous"
   * board-panic alert cannot fire — surface as an honest caveat. True whether
   * the supervisor is off (default) or on-but-no-board-temps.
   */
  dieFallbackCaveat: boolean;
  /** Inter-chip imbalance diagnostic — null unless the supervisor is active. */
  imbalance: ThermalImbalanceView | null;
}

/**
 * Map a raw `/api/thermal/supervisor` snapshot (or null when unreachable) to an
 * honest view model. Never fabricates protection: a disabled/empty snapshot maps
 * to `availability: 'disabled'` with the imbalance diagnostic suppressed.
 */
export function classifyThermalSupervisor(
  snap: ThermalSupervisorSnapshot | null | undefined,
): ThermalSupervisorView {
  if (!snap) {
    return { availability: 'unavailable', dieFallbackCaveat: false, imbalance: null };
  }

  const boards = snap.board_states ?? [];
  // No per-board PCB/chip sensors supervised → only the SoC-die fallback is
  // available for the >=70 °C escalation, which therefore cannot fire.
  const dieFallbackCaveat = boards.length === 0;

  if (!snap.enabled) {
    return { availability: 'disabled', dieFallbackCaveat, imbalance: null };
  }

  const worst = snap.worst_chip_imbalance_c;
  const flagged =
    boards.some(b => b.chip_imbalance_flagged) ||
    (worst !== null && worst !== undefined && worst >= snap.chip_imbalance_threshold_c);

  return {
    availability: 'active',
    dieFallbackCaveat,
    imbalance: {
      worst: worst ?? null,
      threshold: snap.chip_imbalance_threshold_c,
      flagged,
      boards,
    },
  };
}
