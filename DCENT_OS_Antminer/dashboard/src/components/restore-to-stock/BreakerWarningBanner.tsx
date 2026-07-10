// BreakerWarningBanner — sticky breaker/noise reminder.
//
//  W9-G (R5-H4): the original modal placed the breaker warning
// only on step 1. By the time the operator reaches the safety preflight
// or final confirm panels, the warning is no longer visible above the
// fold. This banner is rendered at the TOP of every multi-step screen
// (TypeSerial, NandBackup, SafetyPreflight, Confirm) so the warning
// persists across the flow.
//
// Single source of truth for the breaker copy — if we ever amend the
// message, all 4 step components pick it up automatically.

import React from 'react';
import { InfoDot } from '../common/Tooltip';

export function BreakerWarningBanner() {
  // role="note" + verbatim copy are load-bearing ( W2 left this file
  // untouched on purpose).  P4 changes CHROME ONLY — the role, the
  // ⚠ glyph and every word stay byte-identical; we only add a calm scoped
  // surface class + an optional explainer affordance.
  return (
    <div
      role="note"
      aria-label="Breaker and noise warning"
      className="p4-breaker-note"
      style={bannerStyle}
    >
      <span aria-hidden style={{ marginRight: 8 }}>⚠</span>
      <span>
        <strong>Stock Bitmain firmware is breaker-stressing and noisy.</strong>{' '}
        Keep your hand on the power switch.
      </span>
      <InfoDot term="brick_anxiety" placement="bottom" label="Why this warning is here" />
    </div>
  );
}

const bannerStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: 4,
  padding: '8px 12px',
  borderRadius: 8,
  background: 'rgba(240,180,41,0.10)',
  border: '1px solid rgba(240,180,41,0.32)',
  color: 'var(--text)',
  fontSize: '0.78rem',
  fontWeight: 600,
  marginBottom: 12,
  lineHeight: 1.4,
};
