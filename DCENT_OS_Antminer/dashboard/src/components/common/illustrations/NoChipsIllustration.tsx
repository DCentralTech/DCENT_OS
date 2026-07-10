import React from 'react';

interface IllustrationProps {
  width?: number;
  height?: number;
  className?: string;
}

/**
 * "No chips detected" empty-state illustration.
 * Three BM1387-style chip squares in a row: two solid + one dashed
 * (the missing chip). Pins on top/bottom. Center has an em-dash
 * "—" mark indicating absence.
 */
export function NoChipsIllustration({
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
      data-testid="empty-illustration-no-chips"
      role="img"
      aria-label="No chips detected"
    >
      {/* Left chip (solid) */}
      <rect x="14" y="44" width="28" height="32" rx="2" />
      <circle cx="28" cy="60" r="2" fill="currentColor" stroke="none" />
      {/* Left chip pins */}
      <line x1="20" y1="40" x2="20" y2="44" />
      <line x1="28" y1="40" x2="28" y2="44" />
      <line x1="36" y1="40" x2="36" y2="44" />
      <line x1="20" y1="76" x2="20" y2="80" />
      <line x1="28" y1="76" x2="28" y2="80" />
      <line x1="36" y1="76" x2="36" y2="80" />

      {/* Middle chip — dashed (missing) */}
      <rect
        x="46"
        y="44"
        width="28"
        height="32"
        rx="2"
        strokeDasharray="4 4"
      />
      {/* Em-dash inside the missing slot */}
      <line
        x1="54"
        y1="60"
        x2="66"
        y2="60"
        className="empty-illustration-accent"
        strokeWidth="2.2"
      />
      {/* Middle chip pins (dashed too for consistency) */}
      <line x1="52" y1="40" x2="52" y2="44" strokeDasharray="2 2" />
      <line x1="60" y1="40" x2="60" y2="44" strokeDasharray="2 2" />
      <line x1="68" y1="40" x2="68" y2="44" strokeDasharray="2 2" />
      <line x1="52" y1="76" x2="52" y2="80" strokeDasharray="2 2" />
      <line x1="60" y1="76" x2="60" y2="80" strokeDasharray="2 2" />
      <line x1="68" y1="76" x2="68" y2="80" strokeDasharray="2 2" />

      {/* Right chip (solid) */}
      <rect x="78" y="44" width="28" height="32" rx="2" />
      <circle cx="92" cy="60" r="2" fill="currentColor" stroke="none" />
      {/* Right chip pins */}
      <line x1="84" y1="40" x2="84" y2="44" />
      <line x1="92" y1="40" x2="92" y2="44" />
      <line x1="100" y1="40" x2="100" y2="44" />
      <line x1="84" y1="76" x2="84" y2="80" />
      <line x1="92" y1="76" x2="92" y2="80" />
      <line x1="100" y1="76" x2="100" y2="80" />

      {/* Subtle base line */}
      <line
        x1="14"
        y1="92"
        x2="106"
        y2="92"
        strokeDasharray="2 3"
        className="empty-illustration-dim"
      />
    </svg>
  );
}
