//  W13-D (Item 2): consistent green-check / red-X iconography
// across pre-flight checklist + flash phase rendering. A small,
// dependency-free glyph component that standardizes the four
// operator-facing states the dashboard surfaces over and over:
//
//   ok      → green ✓        (row passed / probe found / phrase matches)
//   fail    → red ✗          (row failed / probe missing / serial mismatch)
//   warn    → amber !        (supported-but-pending-live-test, soft-block)
//   pending → dim ⋯          (probe not run yet / loading skeleton)
//
// Replaces ad-hoc `✓` / `✗` / `!` literals scattered across
// `restore-to-stock/*` and `Confirm.tsx`. Glyphs and color tokens
// match the wave-12 W12-C `<DynamicPreflightChecklist />` legend so
// the operator sees the SAME visual language during preflight,
// staging, and the flash-phase row.
//
// `aria-hidden` because the surrounding row's text label is the real
// accessible signal — screen readers don't need to announce the
// glyph too.

import React from 'react';

export type StatusIconState = 'ok' | 'fail' | 'warn' | 'pending';

interface Props {
  state: StatusIconState;
  /** Glyph cell width, defaults to 16px (matches preflight grid). */
  size?: number;
}

const COLORS: Record<StatusIconState, string> = {
  ok: 'var(--green, #10B981)',
  fail: 'var(--red, #EF4444)',
  warn: 'var(--amber, #F59E0B)',
  pending: 'var(--text-dim, #6E6E80)',
};

const GLYPHS: Record<StatusIconState, string> = {
  ok: '✓',
  fail: '✗',
  warn: '!',
  pending: '⋯',
};

export function StatusIcon({ state, size = 16 }: Props) {
  return (
    <span
      aria-hidden
      data-testid={`status-icon-${state}`}
      data-state={state}
      style={{
        display: 'inline-block',
        minWidth: size,
        color: COLORS[state],
        fontSize: '0.95rem',
        lineHeight: 1.4,
        fontWeight: 700,
        textAlign: 'center',
      }}
    >
      {GLYPHS[state]}
    </span>
  );
}
