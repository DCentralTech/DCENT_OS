import React from 'react';

/**
 * Branded empty-state primitive. Wraps the `.ds-empty-state` design-system
 * class so Standard-mode pages can replace ad-hoc "no data" placeholders
 * with a consistent visual. Hacker mode automatically appends a blinking
 * terminal cursor via the `.mode-hacker .ds-empty-state::after` rule.
 *
 * Keep this lean: it must compile into the 2 MiB inline-HTML budget.
 */
export interface EmptyStateProps {
  icon?: React.ReactNode;
  /**
   * Larger branded illustration rendered ABOVE the title (88px+). Use this
   * for the per-page SVGs in `common/illustrations/`. When `illustration` is
   * supplied it takes precedence over `icon`; the small inline `icon` slot
   * is only used when no illustration is provided.
   */
  illustration?: React.ReactNode;
  title: string;
  hint?: string;
  action?: { label: string; onClick: () => void };
  /**
   *  P5 — OPTIONAL ADDITIVE (D-14). When true the CTA renders as the
   * design-system `primary` button (a "no data → do this" empty state
   * should pull the eye to the resolving action). Default false ⇒ neutral
   * `.ds-btn`, exactly as every existing caller renders today.
   */
  actionPrimary?: boolean;
  /**
   *  P5 — OPTIONAL ADDITIVE (D-30). Whether this empty state should
   * announce as a polite live region (`role="status"`). Default `true`
   * preserves the existing behaviour for every current caller; pages that
   * render a static "nothing here" placeholder on route-change can pass
   * `live={false}` to stop the SR from re-announcing it on every navigation.
   */
  live?: boolean;
  'data-testid'?: string;
}

// Default flame/chip glyph — used when the caller does not supply an icon.
// Plain SVG, no external assets, currentColor so the design-system circle
// colour (`--accent`) shows through.
function DefaultIcon() {
  return (
    <svg
      width="22"
      height="22"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M12 2.5c1.2 2.4 3.5 3.6 3.5 6.5a3.5 3.5 0 1 1-7 0c0-1.4.5-2.2 1.2-3" />
      <path d="M8 13.5C6.5 14.8 5.5 16.5 5.5 18a6.5 6.5 0 0 0 13 0c0-1.4-.5-2.7-1.5-4" />
    </svg>
  );
}

export function EmptyState({
  icon,
  illustration,
  title,
  hint,
  action,
  actionPrimary = false,
  live = true,
  'data-testid': dataTestId,
}: EmptyStateProps) {
  return (
    <div
      className="ds-empty-state"
      data-testid={dataTestId}
      role={live ? 'status' : undefined}
    >
      {illustration ? (
        <span className="ds-empty-illustration" aria-hidden="true">
          {illustration}
        </span>
      ) : (
        <span className="ds-empty-icon" aria-hidden="true">
          {icon ?? <DefaultIcon />}
        </span>
      )}
      <div className="ds-empty-title">{title}</div>
      {hint && <div className="ds-empty-hint">{hint}</div>}
      {action && (
        <button
          type="button"
          className={`ds-btn cp-empty-cta-primary${actionPrimary ? ' primary' : ''}`}
          onClick={action.onClick}
        >
          {action.label}
        </button>
      )}
    </div>
  );
}
