import React from 'react';

/**
 * Branded empty-state for a single `<tr>`. Sibling of `EmptyState` but
 * lives inside a `<tbody>` (where you can't drop a `<div>`). Keeps the
 * visual rhythm — dashed border, accent-glow chip, condensed title —
 * consistent with `ds-empty-state` from design-system.css.
 *
 * Designed to be a drop-in replacement for the recurring pattern:
 *
 *   <tr><td colSpan={N} style={{ ...td, textAlign: 'center',
 *     color: 'var(--text-dim)' }}>No rows.</td></tr>
 */
export interface EmptyTableRowProps {
  colSpan: number;
  title: string;
  hint?: string;
  icon?: React.ReactNode;
  'data-testid'?: string;
}

function DefaultIcon() {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M3 6h18M3 12h18M3 18h18" opacity=".35" />
      <circle cx="12" cy="12" r="9" />
    </svg>
  );
}

export function EmptyTableRow({
  colSpan,
  title,
  hint,
  icon,
  'data-testid': dataTestId,
}: EmptyTableRowProps) {
  return (
    <tr data-testid={dataTestId}>
      <td
        colSpan={colSpan}
        style={{
          padding: '24px 16px',
          textAlign: 'center',
          color: 'var(--fg-secondary, var(--text-dim, #8b8b9e))',
          background: 'rgba(255,255,255,0.012)',
          fontSize: '0.82rem',
        }}
      >
        <div
          style={{
            display: 'inline-flex',
            flexDirection: 'column',
            alignItems: 'center',
            gap: 8,
          }}
        >
          <span
            className="ds-empty-icon"
            aria-hidden="true"
            style={{
              width: 32,
              height: 32,
              borderRadius: '50%',
              background: 'var(--accent-glow, rgba(250,165,0,.15))',
              border: '1px solid var(--accent-border, rgba(250,165,0,.32))',
              display: 'inline-flex',
              alignItems: 'center',
              justifyContent: 'center',
              color: 'var(--accent, #FAA500)',
            }}
          >
            {icon ?? <DefaultIcon />}
          </span>
          <div
            style={{
              fontFamily: "var(--font-heading)",
              fontWeight: 700,
              fontSize: '0.95rem',
              color: 'var(--fg-primary, var(--text, #f0f0f0))',
              letterSpacing: '.01em',
            }}
          >
            {title}
          </div>
          {hint && (
            <div
              style={{
                fontSize: '0.75rem',
                color: 'var(--fg-secondary, var(--text-dim, #8b8b9e))',
                maxWidth: '40ch',
                lineHeight: 1.5,
              }}
            >
              {hint}
            </div>
          )}
        </div>
      </td>
    </tr>
  );
}
