import React from 'react';

interface IllustrationProps {
  width?: number;
  height?: number;
  className?: string;
}

/**
 * "No pool configured / pool disconnected" empty-state illustration.
 * Hand-drawn-feel SVG matching the existing icon system (1.6 stroke,
 * fill="none", currentColor). Pairs nicely with the warm accent
 * (.empty-illustration sets color to --accent).
 *
 * Composition: a server/rack icon with a broken cable wave passing
 * through it and a small "?" mark indicating the missing connection.
 */
export function NoPoolIllustration({
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
      data-testid="empty-illustration-no-pool"
      role="img"
      aria-label="No pool configured"
    >
      {/* Server / rack body */}
      <rect x="30" y="36" width="60" height="48" rx="4" />
      {/* Rack rows */}
      <line x1="30" y1="52" x2="90" y2="52" />
      <line x1="30" y1="68" x2="90" y2="68" />
      {/* Status LEDs */}
      <circle cx="40" cy="44" r="1.6" fill="currentColor" stroke="none" />
      <circle cx="46" cy="44" r="1.6" fill="currentColor" stroke="none" />
      <circle cx="40" cy="60" r="1.6" fill="currentColor" stroke="none" />
      <circle cx="46" cy="60" r="1.6" fill="currentColor" stroke="none" />
      <circle cx="40" cy="76" r="1.6" fill="currentColor" stroke="none" />
      <circle cx="46" cy="76" r="1.6" fill="currentColor" stroke="none" />
      {/* Broken cable wave — left segment */}
      <path d="M8 96 Q 18 86 28 96 T 48 96" strokeDasharray="0" />
      {/* Gap (broken) */}
      <line x1="52" y1="92" x2="56" y2="100" className="empty-illustration-accent" />
      <line x1="56" y1="92" x2="52" y2="100" className="empty-illustration-accent" />
      {/* Cable wave — right segment (dashed = disconnected) */}
      <path
        d="M64 96 Q 74 86 84 96 T 112 96"
        strokeDasharray="3 4"
      />
      {/* Question mark */}
      <circle cx="84" cy="22" r="9" />
      <path d="M81 19 Q 84 15 87 19 T 84 25" />
      <circle cx="84" cy="28.5" r="1.1" fill="currentColor" stroke="none" />
      {/* Small accent dot */}
      <circle cx="100" cy="40" r="1.8" fill="currentColor" stroke="none" />
    </svg>
  );
}
