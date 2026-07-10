import React from 'react';

interface IllustrationProps {
  width?: number;
  height?: number;
  className?: string;
}

/**
 * "No miners in fleet" empty-state illustration.
 * Three stylized mining-rig boxes (chassis + fan-grille + LEDs) in a
 * row, with the rightmost rig ghosted/dashed to indicate "add more".
 */
export function NoFleetIllustration({
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
      data-testid="empty-illustration-no-fleet"
      role="img"
      aria-label="No miners in fleet"
    >
      {/* Rig 1 (solid) */}
      <rect x="8" y="44" width="30" height="44" rx="3" />
      {/* fan grille — concentric circles */}
      <circle cx="23" cy="58" r="7" />
      <circle cx="23" cy="58" r="3" />
      <circle cx="23" cy="58" r="1" fill="currentColor" stroke="none" />
      {/* status LEDs */}
      <circle cx="16" cy="80" r="1.4" fill="currentColor" stroke="none" />
      <circle cx="22" cy="80" r="1.4" fill="currentColor" stroke="none" />
      <circle cx="28" cy="80" r="1.4" fill="currentColor" stroke="none" />

      {/* Rig 2 (solid) */}
      <rect x="45" y="44" width="30" height="44" rx="3" />
      <circle cx="60" cy="58" r="7" />
      <circle cx="60" cy="58" r="3" />
      <circle cx="60" cy="58" r="1" fill="currentColor" stroke="none" />
      <circle cx="53" cy="80" r="1.4" fill="currentColor" stroke="none" />
      <circle cx="59" cy="80" r="1.4" fill="currentColor" stroke="none" />
      <circle cx="65" cy="80" r="1.4" fill="currentColor" stroke="none" />

      {/* Rig 3 — dashed (ghosted "add me") */}
      <rect
        x="82"
        y="44"
        width="30"
        height="44"
        rx="3"
        strokeDasharray="4 4"
      />
      <circle cx="97" cy="58" r="7" strokeDasharray="3 3" />
      <circle cx="97" cy="58" r="3" strokeDasharray="2 2" />
      {/* Plus sign in the empty slot */}
      <line
        x1="97"
        y1="76"
        x2="97"
        y2="84"
        className="empty-illustration-accent"
        strokeWidth="2"
      />
      <line
        x1="93"
        y1="80"
        x2="101"
        y2="80"
        className="empty-illustration-accent"
        strokeWidth="2"
      />

      {/* Floor line connecting the rack */}
      <line
        x1="4"
        y1="94"
        x2="116"
        y2="94"
        strokeDasharray="2 3"
        className="empty-illustration-dim"
      />
    </svg>
  );
}
