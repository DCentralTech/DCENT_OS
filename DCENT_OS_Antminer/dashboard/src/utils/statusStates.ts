// Status / truth-ladder state strings — STRING SOURCE (the canonical data
// layer). DCENT Design Language — Terminology Lexicon, TERM-2 + §7 (StatusPill
// state → label map). Source of truth:
// docs/design-system/DCENT_DESIGN_LANGUAGE/terminology-lexicon.md.
//
// WHY THIS LIVES IN THE STRING SOURCE (and not the pill component):
//   The lexicon owns the WORD; the tokens contract owns the COLOR; the
//   StatusPill component (wave 2) renders {state} → label. Centralizing the
//   canonical labels here means wave-2 components import them instead of
//   inlining literals, so the truth ladder can never drift between the topbar
//   chip, the favicon state, and the pills.
//
// THE KEYSTONE RECONCILIATION (lexicon §2.1):
//   rung 0  telemetry_pending  "Telemetry pending"
//   rung 1  mining             "Mining"            (enabled AND positive hashrate)
//   rung 2  ready              "Ready"  ← canonical id `ready` (NOT `enabled`):
//                                          permitted but hashrate still zero/unknown
//   rung 3  standby            "Standby" ← canonical id `standby`: not mining
//
// `health.ts` :: minerChip currently emits Connecting / Offline / Stale data /
// Mining / Standby inline and LACKS the rung-2 "Ready" distinction (it lumps
// permitted-but-zero-hashrate into Standby/Connecting). This module defines the
// canonical "Ready" rung label + a pure helper so wave 2 can wire the rung into
// the pill WITHOUT changing minerChip's existing return values this phase
// (pure-additive — no live behavior change here).
//
// Pure data + pure functions. No .tsx import. Additive — nothing imports it yet.

import type { GlossaryKey } from './glossary';

/**
 * The canonical StatusPill state ids (lexicon §7).  `StatusPill` maps a
 * state id → label via `STATUS_STATE_LABELS` and → a tone role via the tokens
 * contract. `ready` is the rung-2 id; `standby` is rung-3 — these two ids are
 * load-bearing and pinned by the lexicon.
 */
export type StatusStateId =
  | 'telemetry_pending'
  | 'mining'
  | 'ready'
  | 'standby'
  | 'stopped'
  | 'connecting'
  | 'connected'
  | 'online'
  | 'warning'
  | 'error'
  | 'offline';

/**
 * Canonical state id → Title-case label (lexicon §7). OS renders Title case
 * (the pill CSS uppercases via letter-spacing); the underlying string is Title
 * case. axe renders raw UPPERCASE literals — same WORD, per-substrate casing.
 */
export const STATUS_STATE_LABELS: Readonly<Record<StatusStateId, string>> = {
  telemetry_pending: 'Telemetry pending',
  mining: 'Mining',
  ready: 'Ready',
  standby: 'Standby',
  stopped: 'Stopped',
  connecting: 'Connecting',
  connected: 'Connected',
  online: 'Online',
  warning: 'Warning',
  error: 'Error',
  offline: 'Offline',
} as const;

/**
 * Canonical state id → glossary key, so a pill / chip can resolve a rich
 * truth-contract tooltip from the same source as its label. (offline maps to
 * `telemetry_absent` per lexicon §7.)
 */
export const STATUS_STATE_GLOSSARY: Readonly<Record<StatusStateId, GlossaryKey>> = {
  telemetry_pending: 'state_telemetry_pending',
  mining: 'state_mining',
  ready: 'state_ready',
  standby: 'state_standby',
  stopped: 'state_stopped',
  connecting: 'pool_connecting',
  connected: 'pool_connected',
  online: 'state_online',
  warning: 'state_warning',
  error: 'state_error',
  offline: 'telemetry_absent',
} as const;

/** Resolve a canonical state id to its label. */
export function statusStateLabel(state: StatusStateId): string {
  return STATUS_STATE_LABELS[state];
}

// Convenience individual exports for the load-bearing rungs so wave-2 callers
// can import the exact canonical strings by name.
export const STATE_LABEL_TELEMETRY_PENDING = STATUS_STATE_LABELS.telemetry_pending;
export const STATE_LABEL_MINING = STATUS_STATE_LABELS.mining;
/** Rung-2 canonical label (id `ready`). Permitted, hashrate still zero/unknown. */
export const STATE_LABEL_READY = STATUS_STATE_LABELS.ready;
/** Rung-3 canonical label (id `standby`). Mining disabled / not running. */
export const STATE_LABEL_STANDBY = STATUS_STATE_LABELS.standby;
export const STATE_LABEL_STOPPED = STATUS_STATE_LABELS.stopped;

/**
 * Pure rung-2 vs rung-3 classifier for the mining-state ladder (lexicon §2.1).
 * Given "is mining permitted/enabled" and "is a positive hashrate visible",
 * returns the canonical state id:
 *   - mining enabled + positive hashrate          → `mining`   (rung 1)
 *   - mining enabled + zero/unknown hashrate       → `ready`    (rung 2)
 *   - mining disabled                              → `standby`  (rung 3)
 *
 * This is the helper that distinguishes permitted-zero-hashrate ("Ready") from
 * stopped ("Standby") — the distinction `health.ts :: minerChip` does not draw
 * today.  wires this into the pill; it is offered here as the canonical,
 * unit-testable rung logic, and it does NOT mutate any existing behavior.
 */
export function classifyMiningState(
  miningEnabled: boolean,
  hasPositiveHashrate: boolean,
): Extract<StatusStateId, 'mining' | 'ready' | 'standby'> {
  if (!miningEnabled) return 'standby';
  return hasPositiveHashrate ? 'mining' : 'ready';
}
