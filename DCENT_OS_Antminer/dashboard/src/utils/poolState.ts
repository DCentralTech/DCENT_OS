// PoolState → StatusState projection — STRING/LOGIC SOURCE (the canonical data
// layer). DCENT Design Language — COMPONENT CONTRACT §6 (COMP-POOLCARD).
// Source of truth: docs/design-system/DCENT_DESIGN_LANGUAGE/component-contract.md.
//
// WHY THIS LIVES IN A PURE UTIL (and not inline in PoolConfig.tsx):
//   The §6 PoolCard owns its OWN connection truth-ladder (`PoolState`):
//
//     connecting → connected → authorized → job_fresh → mining_capable → failover
//
//   The embedded liveness pill (COMP-PILL, §1) renders ONLY `StatusState`
//   enum values, so every `PoolState` rung MUST project onto exactly one
//   `StatusState`. That projection was previously inlined in
//   components/standard/PoolConfig.tsx, so the §6 table existed in only one
//   place and components/common/PoolStatus.tsx had a SECOND, ad-hoc projection.
//   Lifting the pure rung→state switch here gives BOTH surfaces ONE projection,
//   so `connecting ≠ connected ≠ mining_capable` can never drift between them.
//
// THE §6 PROJECTION TABLE (component-contract.md §6, verbatim):
//   PoolState rung    → StatusState (pill)    tone role
//   ─────────────────   ──────────────────     ─────────
//   connecting        → connecting            info/warn
//   connected         → online                ok
//   authorized        → online                ok
//   job_fresh         → online                ok
//   mining_capable    → mining                ok   (the ONLY rung claiming `mining`)
//   failover          → warning               warn
//
//   `connected/authorized/job_fresh` collapse to `online` (reachable / session
//   up, not yet hashing); `mining_capable` is the only rung that claims
//   `mining`. The pill NEVER renders a raw `PoolState` token (it is not in the
//   `StatusState` enum) — this projection is the only way a `PoolState` reaches
//   a pill.
//
// Pure data + pure functions. No .tsx import. Additive — re-exported into
// PoolConfig.tsx + PoolStatus.tsx so both consume ONE projection.

import type { StatusState } from '../components/common/StatusPill';

/**
 * The §6 PoolCard connection truth-ladder (closed, ORDERED). This is the
 * PoolCard's own connection ladder — it is NOT the `StatusState` enum; it
 * projects onto `StatusState` via `poolStateToStatusState` below.
 */
export type PoolStateRung =
  | 'connecting'
  | 'connected'
  | 'authorized'
  | 'job_fresh'
  | 'mining_capable'
  | 'failover';

/**
 * Project a §6 `PoolState` rung onto the canonical `StatusState` the embedded
 * COMP-PILL renders. This is the byte-equivalent move of the projection that
 * previously lived inline in PoolConfig.tsx:125-138 — the rung→state mapping
 * is preserved exactly so the  `connecting ≠ connected ≠ mining_capable`
 * truth contract (which rest.rs predicates back) does not regress:
 *
 *   connecting     → connecting (info/warn — NOT connected)
 *   connected      → online
 *   authorized     → online
 *   job_fresh      → online
 *   mining_capable → mining     (the only rung that claims `mining`)
 *   failover       → warning
 */
export function poolStateToStatusState(rung: PoolStateRung): StatusState {
  switch (rung) {
    case 'connecting':
      return 'connecting';
    case 'mining_capable':
      return 'mining';
    case 'failover':
      return 'warning';
    case 'connected':
    case 'authorized':
    case 'job_fresh':
    default:
      return 'online';
  }
}
