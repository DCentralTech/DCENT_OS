import React from 'react';

interface SkeletonProps {
  width?: string | number;
  height?: string | number;
  borderRadius?: string | number;
  style?: React.CSSProperties;
  className?: string;
}

/**
 * Single shimmer block. Powered by the canonical `.ds-skel` class shipped
 * by Agent 1 in design-system.css (1.4s `ds-shimmer` keyframe). Footprint
 * is one element + an inline width/height/radius override.
 */
export function Skeleton({ width = '100%', height = 16, borderRadius = 6, style, className }: SkeletonProps) {
  return (
    <span
      className={`ds-skel${className ? ' ' + className : ''}`}
      aria-hidden="true"
      style={{
        width,
        height,
        borderRadius,
        display: 'block',
        ...style,
      }}
    />
  );
}

/** A full dashboard skeleton placeholder showing pulsing bars in a KPI-like layout */
export function DashboardSkeleton() {
  return (
    <div style={{ padding: 24 }} role="status" aria-busy="true" aria-live="polite">
      <span className="sr-only">Loading dashboard telemetry</span>
      {/* Top bar skeleton */}
      <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 24 }}>
        <Skeleton width={180} height={24} />
        <div style={{ display: 'flex', gap: 24 }}>
          <Skeleton width={80} height={24} />
          <Skeleton width={80} height={24} />
          <Skeleton width={80} height={24} />
          <Skeleton width={80} height={24} />
        </div>
      </div>

      {/* KPI row skeleton — Wave-13: auto-fill (was fixed repeat(4,1fr) which
          produced four ~80px columns too narrow at 375px). */}
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(min(100%, 140px), 1fr))', gap: 16, marginBottom: 24 }}>
        {Array.from({ length: 4 }).map((_, i) => (
          <div key={i} style={{
            padding: 16,
            borderRadius: 12,
            background: 'rgba(255,255,255,0.02)',
            border: '1px solid rgba(255,255,255,0.05)',
          }}>
            <Skeleton width={80} height={12} style={{ marginBottom: 8 }} />
            <Skeleton width={120} height={28} />
          </div>
        ))}
      </div>

      {/* Chart skeleton */}
      <div style={{
        padding: 16,
        borderRadius: 12,
        background: 'rgba(255,255,255,0.02)',
        border: '1px solid rgba(255,255,255,0.05)',
        marginBottom: 24,
      }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 16 }}>
          <Skeleton width={160} height={20} />
          <Skeleton width={120} height={20} />
        </div>
        <Skeleton width="100%" height={200} borderRadius={8} />
      </div>

      {/* Stats grid skeleton */}
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(180px, 1fr))', gap: 12 }}>
        {Array.from({ length: 6 }).map((_, i) => (
          <div key={i} style={{
            padding: 14,
            borderRadius: 8,
            background: 'rgba(255,255,255,0.02)',
            border: '1px solid rgba(255,255,255,0.05)',
          }}>
            <Skeleton width={60} height={12} style={{ marginBottom: 8 }} />
            <Skeleton width={100} height={22} />
          </div>
        ))}
      </div>

    </div>
  );
}
