import React from 'react';

interface PageSkeletonProps {
  /** Optional override for the data-testid (defaults to `skeleton-page`). */
  'data-testid'?: string;
}

/**
 * Full-page loading state. Mirrors the canonical `.page-hero-strip` layout:
 * a hero-summary block on the left + a 4-column KPI strip on the right,
 * followed by 2 fake content sections. Uses `.ds-skel` shimmer (from
 * design-system.css) and the `.skeleton-*` classes (in canonical.css).
 *
 * Render this while a page's primary data is loading. Once data resolves,
 * the actual page composes in with the design-system's `ds-fadeIn` easing.
 */
export function PageSkeleton({
  'data-testid': testId = 'skeleton-page',
}: PageSkeletonProps = {}) {
  return (
    <div
      className="skeleton-page"
      data-testid={testId}
      aria-busy="true"
      aria-live="polite"
      role="status"
    >
      <span className="sr-only">Loading…</span>

      {/* Hero strip: summary on the left + KPI grid on the right */}
      <div className="skeleton-hero">
        <div className="skeleton-hero-summary">
          {/* eyebrow */}
          <span
            className="ds-skel"
            style={{ width: 96, height: 10 }}
          />
          {/* title */}
          <span
            className="ds-skel"
            style={{ width: '78%', height: 22, marginTop: 4 }}
          />
          {/* sub-title / status */}
          <span
            className="ds-skel"
            style={{ width: '56%', height: 12, marginTop: 6 }}
          />
          {/* spacer + 2 action chips */}
          <div style={{ display: 'flex', gap: 8, marginTop: 12 }}>
            <span
              className="ds-skel"
              style={{ width: 88, height: 28, borderRadius: 999 }}
            />
            <span
              className="ds-skel"
              style={{ width: 64, height: 28, borderRadius: 999 }}
            />
          </div>
        </div>

        <div className="skeleton-kpi-strip">
          {Array.from({ length: 4 }).map((_, i) => (
            <div key={i} className="skeleton-kpi" />
          ))}
        </div>
      </div>

      {/* Section 1 */}
      <section className="skeleton-section">
        <span className="ds-skel title" />
        <span className="ds-skel row long" />
        <span className="ds-skel row mid" />
        <span className="ds-skel row long" />
        <span className="ds-skel row short" />
      </section>

      {/* Section 2 */}
      <section className="skeleton-section">
        <span className="ds-skel title" />
        <span className="ds-skel row mid" />
        <span className="ds-skel row long" />
        <span className="ds-skel row mid" />
      </section>
    </div>
  );
}
