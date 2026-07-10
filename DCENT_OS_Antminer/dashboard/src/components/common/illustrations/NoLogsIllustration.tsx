import React from 'react';

interface IllustrationProps {
  width?: number;
  height?: number;
  className?: string;
}

/**
 * "No logs yet" empty-state illustration.
 * A retro terminal window with traffic-light header dots and a
 * `> _` prompt where the underscore cursor blinks via <animate>.
 */
export function NoLogsIllustration({
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
      data-testid="empty-illustration-no-logs"
      role="img"
      aria-label="No logs yet"
    >
      {/* Terminal window outline */}
      <rect x="14" y="24" width="92" height="72" rx="6" />
      {/* Title bar */}
      <line x1="14" y1="38" x2="106" y2="38" />
      {/* Traffic light dots */}
      <circle cx="22" cy="31" r="2" fill="currentColor" stroke="none" />
      <circle cx="30" cy="31" r="2" fill="currentColor" stroke="none" />
      <circle cx="38" cy="31" r="2" fill="currentColor" stroke="none" />
      {/* Prompt: >  */}
      <polyline points="24,56 32,64 24,72" />
      {/* Blinking cursor */}
      <line
        x1="40"
        y1="72"
        x2="56"
        y2="72"
        className="empty-illustration-accent"
        strokeWidth="2.2"
      >
        <animate
          attributeName="opacity"
          values="1;1;0;0"
          keyTimes="0;0.5;0.5;1"
          dur="1.1s"
          repeatCount="indefinite"
        />
      </line>
      {/* A few faint guide lines hinting at "empty log area" */}
      <line
        x1="24"
        y1="82"
        x2="76"
        y2="82"
        strokeDasharray="2 4"
        className="empty-illustration-dim"
      />
      <line
        x1="24"
        y1="90"
        x2="64"
        y2="90"
        strokeDasharray="2 4"
        className="empty-illustration-dim"
      />
    </svg>
  );
}
