import React from 'react';

interface SectionSkeletonProps {
  /** Number of content rows to render (default 3). */
  rows?: number;
  /** Optional override for the data-testid (defaults to `skeleton-section`). */
  'data-testid'?: string;
}

/**
 * A single fake content section, useful inline (inside an already-rendered
 * page) when one specific card or panel is still loading. Renders a title
 * placeholder followed by alternating row widths so multiple instances
 * don't look like a uniform grid.
 */
export function SectionSkeleton({
  rows = 3,
  'data-testid': testId = 'skeleton-section',
}: SectionSkeletonProps = {}) {
  // Alternate widths so successive rows feel naturally hand-drawn.
  const widthClasses: Array<'long' | 'mid' | 'short'> = ['long', 'mid', 'short'];

  return (
    <section
      className="skeleton-section"
      data-testid={testId}
      aria-busy="true"
      aria-live="polite"
      role="status"
    >
      <span className="sr-only">Loading…</span>
      <span className="ds-skel title" />
      {Array.from({ length: Math.max(1, rows) }).map((_, i) => {
        const w = widthClasses[i % widthClasses.length];
        return <span key={i} className={`ds-skel row ${w}`} />;
      })}
    </section>
  );
}
