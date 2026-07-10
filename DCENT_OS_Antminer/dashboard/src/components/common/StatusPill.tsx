import React from 'react';
import { glossary, type GlossaryKey } from '../../utils/glossary';

/**
 * Canonical cross-firmware status truth-ladder (DCENT Design Language —
 * component-contract.md §1 COMP-PILL). This is the SUPERSET of the legacy OS
 * union (`online | warning | error | offline | unknown`): every previously
 * accepted value still compiles (the widen is strictly additive), and the new
 * rungs `telemetry_pending | connecting | mining | ready | stopped | standby`
 * become first-class so the pill can express the full honesty ladder instead
 * of folding everything into `online`/`unknown`.
 *
 * Truth-ladder (contract §1):
 *   telemetry_pending — pre-first-data; FIRST PAINT must be this, never `mining`
 *   connecting        — TCP/handshake in progress; NOT connected
 *   mining            — enabled AND positive hashrate visible
 *   ready             — mining permitted but hashrate still zero/unknown
 *   standby / stopped — mining disabled / explicitly stopped
 *   online            — device reachable / management-up (non-mining liveness)
 *   warning           — degraded but operational
 *   error             — fault
 *   offline           — unreachable
 *
 * The canonical rung-2 id is `ready` (NOT `enabled`); rung-3 is `standby`.
 */
export type StatusState =
  | 'telemetry_pending'
  | 'connecting'
  | 'mining'
  | 'ready'
  | 'standby'
  | 'stopped'
  | 'online'
  | 'warning'
  | 'error'
  | 'offline';

/**
 * Legacy `unknown` stays accepted (every existing caller passes one of
 * `online | warning | error | offline | unknown`); it maps to the
 * telemetry-pending tone (muted) and resolves through the same projection.
 */
export type StatusPillState = StatusState | 'unknown';

/**
 * Tone projection (component-contract §1 tone roles). The OS glass skin's
 * `.cp-status-pill[data-status=*]` CSS only tints the 5 existing buckets
 * (`online | warning | error | offline | unknown`); rather than touch global
 * CSS, every canonical StatusState projects onto one of those buckets for the
 * rendered `data-status`, while the precise canonical id is carried separately
 * on `data-state` for SR/tests/future bespoke CSS. This keeps the change
 * additive (zero CSS edits) and the build green.
 */
type ToneBucket = 'online' | 'warning' | 'error' | 'offline' | 'unknown';

const STATE_TONE: Record<StatusPillState, ToneBucket> = {
  // ok / active rungs
  mining: 'online',
  online: 'online',
  // permitted-but-zero-hashrate → info/ok-dim → warning tint
  ready: 'warning',
  // connection-in-progress → info/warn
  connecting: 'warning',
  warning: 'warning',
  // fault
  error: 'error',
  // unreachable
  offline: 'offline',
  // muted rungs (no bespoke CSS tone today → default/unknown tint)
  standby: 'unknown',
  stopped: 'unknown',
  telemetry_pending: 'unknown',
  unknown: 'unknown',
};

/**
 * Each canonical state's label-source glossary key (terminology contract).
 * When no explicit `label` prop is supplied, the visible terse text + the
 * aria-label resolve from here so labels are NEVER hard-coded in the component
 * (component-contract §0 hard rule). A missing key degrades to the raw state id
 * via `glossary(...)?.term ?? state`, so this is non-fatal by construction.
 */
const STATE_LABEL_KEY: Record<StatusPillState, GlossaryKey | undefined> = {
  telemetry_pending: 'state_telemetry_pending',
  connecting: 'pool_connecting',
  mining: 'state_mining',
  ready: 'state_ready',
  standby: 'state_standby',
  stopped: 'state_stopped',
  online: 'state_online',
  warning: 'state_warning',
  error: 'state_error',
  offline: 'telemetry_absent',
  unknown: 'state_telemetry_pending',
};

function labelForState(state: StatusPillState): string {
  const key = STATE_LABEL_KEY[state];
  return (key && glossary(key)?.term) || state;
}

interface StatusPillProps {
  /**
   * Canonical truth-ladder state. Accepts the full `StatusState` enum plus the
   * legacy `unknown` value (strict superset of the old closed union, so every
   * existing caller compiles unchanged).
   */
  status: StatusPillState;
  label?: string;
  pulse?: boolean;
}

export function StatusPill({ status, label, pulse }: StatusPillProps) {
  // SR-meaningful announcement: the visible label may be terse ("ONLINE")
  // or omitted; surface the semantic state explicitly. Truth contract:
  // aria-label MUST match the visible state. When `label` is omitted the
  // visible text + aria-label resolve from the terminology glossary (never an
  // inlined literal); when `label` IS passed it is preserved byte-for-byte so
  // every existing caller renders exactly as before.
  const resolvedLabel = label ?? labelForState(status);
  const ariaLabel = `Status: ${resolvedLabel}`;
  // Render `data-status` as one of the 5 CSS-backed tone buckets (keeps the
  // glass-pill tint working with no CSS change), while `data-state` carries the
  // precise canonical id for SR/test/future-CSS.
  const tone = STATE_TONE[status];
  return (
    <span
      className="cp-status-pill"
      role="status"
      aria-label={ariaLabel}
      data-status={tone}
      data-state={status}
      data-pulse={pulse ? 'true' : undefined}
    >
      <span className="cp-dot" aria-hidden="true" />
      {resolvedLabel}
    </span>
  );
}
