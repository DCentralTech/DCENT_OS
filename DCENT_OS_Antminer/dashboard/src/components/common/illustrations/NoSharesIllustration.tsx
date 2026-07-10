import React from 'react';

interface IllustrationProps {
  width?: number;
  height?: number;
  className?: string;
}

/**
 * "No shares yet" empty-state illustration.
 * Stack of share-receipt rectangles with a magnifying glass overlay.
 * Top receipt has a dashed border to convey "waiting for the next one".
 */
export function NoSharesIllustration({
  width = 88,
  height = 88,
  className,
}: IllustrationProps) {
  return (
    <svg
      width={width}
      height={height}
      viewBox="0 0 120 120"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={['empty-illustration', className].filter(Boolean).join(' ')}
      data-testid="empty-illustration-no-shares"
      role="img"
      aria-label="No shares yet"
    >
      {/* Bottom receipt */}
      <rect x="22" y="74" width="58" height="22" rx="3" />
      <line x1="30" y1="82" x2="56" y2="82" />
      <line x1="30" y1="88" x2="48" y2="88" />
      {/* Middle receipt (offset up + right) */}
      <rect x="28" y="56" width="58" height="22" rx="3" />
      <line x1="36" y1="64" x2="62" y2="64" />
      <line x1="36" y1="70" x2="54" y2="70" />
      {/* Top receipt — dashed (the missing one) */}
      <rect
        x="34"
        y="38"
        width="58"
        height="22"
        rx="3"
        strokeDasharray="4 4"
      />
      <line x1="42" y1="46" x2="68" y2="46" strokeDasharray="2 3" />
      <line x1="42" y1="52" x2="60" y2="52" strokeDasharray="2 3" />
      {/* Magnifying glass */}
      <circle cx="86" cy="32" r="13" className="empty-illustration-accent" />
      <line
        x1="95.5"
        y1="41.5"
        x2="106"
        y2="52"
        className="empty-illustration-accent"
        strokeWidth="2.2"
      />
      {/* Glass highlight */}
      <path d="M78 28 Q 82 24 86 24" />
    </svg>
  );
}
