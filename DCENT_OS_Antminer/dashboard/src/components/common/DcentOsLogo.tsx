import React from 'react';

/* ─────────────────────────────────────────────────────────────────────────
   DCENT_OS logo — the D-Central 3-sphere molecule (its nodes orbit/spin as a
   rigid group around the centroid) + the DCENT_OS wordmark.

   Clean/modern: NO chip package or pins (that casing read as busy). The bare
   spinning molecule is the brand mark; the orange spheres + orange underscore
   are the only color, the wordmark stays crisp white. Honors
   prefers-reduced-motion (spin disabled). Shared gradient/keyframe ids are
   suffixed so the full logo and the icon can coexist on one page.
   ───────────────────────────────────────────────────────────────────────── */

// Centroid of the three node centres (36,50)/(70,50)/(53,78) ≈ (53, 59).
const MOL_NODES = [
  { x: 36, y: 50, r: 14 },
  { x: 70, y: 50, r: 14 },
  { x: 53, y: 78, r: 14 },
];

function Molecule({ idp, originX, originY }: { idp: string; originX: number; originY: number }) {
  return (
    <g
      className={`${idp}-mol`}
      style={{
        transformBox: 'view-box',
        transformOrigin: `${originX}px ${originY}px`,
      } as React.CSSProperties}
    >
      {/* bonds (triangle) under the nodes */}
      <g stroke="#0a0a0f" strokeWidth="3" strokeLinecap="round" opacity="0.5">
        <line x1={MOL_NODES[0].x} y1={MOL_NODES[0].y} x2={MOL_NODES[1].x} y2={MOL_NODES[1].y} />
        <line x1={MOL_NODES[0].x} y1={MOL_NODES[0].y} x2={MOL_NODES[2].x} y2={MOL_NODES[2].y} />
        <line x1={MOL_NODES[1].x} y1={MOL_NODES[1].y} x2={MOL_NODES[2].x} y2={MOL_NODES[2].y} />
      </g>
      {/* nodes — fill + glossy highlight */}
      {MOL_NODES.map((n, i) => (
        <g key={i}>
          <circle cx={n.x} cy={n.y} r={n.r} fill={`url(#${idp}-sphere)`} filter={`url(#${idp}-glow)`} />
          <circle cx={n.x} cy={n.y} r={n.r} fill={`url(#${idp}-shine)`} />
        </g>
      ))}
    </g>
  );
}

/**
 * DCENT_OS full logo — spinning molecule + wordmark. Pass width to scale
 * (aspect ratio 340:120, unchanged from the prior logo so callers don't shift).
 */
export function DcentOsLogo({ width = 170 }: { width?: number }) {
  const height = Math.round(width * (120 / 340));
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 340 120"
      width={width}
      height={height}
      role="img"
      aria-label="DCENT_OS logo"
    >
      <defs>
        <radialGradient id="logo-sphere" cx="38%" cy="28%" r="65%">
          <stop offset="0%" stopColor="#FFD47A" />
          <stop offset="55%" stopColor="#FAA500" />
          <stop offset="100%" stopColor="#FA6700" />
        </radialGradient>
        <radialGradient id="logo-shine" cx="36%" cy="26%" r="32%">
          <stop offset="0%" stopColor="#fff" stopOpacity={0.72} />
          <stop offset="100%" stopColor="#fff" stopOpacity={0} />
        </radialGradient>
        <filter id="logo-glow" x="-40%" y="-40%" width="180%" height="180%">
          <feGaussianBlur in="SourceAlpha" stdDeviation="3.5" result="blur" />
          <feFlood floodColor="#FAA500" floodOpacity={0.28} result="color" />
          <feComposite in="color" in2="blur" operator="in" result="glow" />
          <feMerge>
            <feMergeNode in="glow" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
        <filter id="logo-textglow">
          <feGaussianBlur stdDeviation="1.4" result="b" />
          <feMerge>
            <feMergeNode in="b" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
        <style>{`
          @keyframes logo-mol-spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
          .logo-mol { animation: logo-mol-spin 14s linear infinite; }
          @media (prefers-reduced-motion: reduce) { .logo-mol { animation: none; } }
        `}</style>
      </defs>

      {/* Spinning molecule — the three nodes orbit as one rigid group. */}
      <Molecule idp="logo" originX={53} originY={59} />

      {/* Wordmark — crisp white DCENT + OS with a single orange underscore accent. */}
      <text
        x="116" y="74"
        fontFamily="-apple-system, 'SF Pro Display', 'Segoe UI', Helvetica, sans-serif"
        fontSize="44" fontWeight="800" letterSpacing="-1.5"
        fill="#f5f5f7" filter="url(#logo-textglow)"
      >
        DCENT
      </text>
      {/* accent underscore — follows the theme accent (see design-system.css) */}
      <rect x="246" y="68" width="16" height="4" rx="2" fill="#FAA500" className="dcent-logo-underscore" />
      <text
        x="266" y="74"
        fontFamily="-apple-system, 'SF Pro Display', 'Segoe UI', Helvetica, sans-serif"
        fontSize="44" fontWeight="800" letterSpacing="-1"
        fill="#f5f5f7" filter="url(#logo-textglow)"
      >
        OS
      </text>
    </svg>
  );
}

/**
 * DCENT_OS mark only — the spinning molecule, for sidebars / favicon / small
 * spaces. No chip, no wordmark.
 */
export function DcentOsIcon({ size = 32 }: { size?: number }) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="20 26 66 66"
      width={size}
      height={size}
      role="img"
      aria-label="DCENT_OS"
    >
      <defs>
        <radialGradient id="icon-sphere" cx="38%" cy="28%" r="65%">
          <stop offset="0%" stopColor="#FFD47A" />
          <stop offset="55%" stopColor="#FAA500" />
          <stop offset="100%" stopColor="#FA6700" />
        </radialGradient>
        <radialGradient id="icon-shine" cx="36%" cy="26%" r="32%">
          <stop offset="0%" stopColor="#fff" stopOpacity={0.72} />
          <stop offset="100%" stopColor="#fff" stopOpacity={0} />
        </radialGradient>
        <filter id="icon-glow" x="-40%" y="-40%" width="180%" height="180%">
          <feGaussianBlur in="SourceAlpha" stdDeviation="3.5" result="blur" />
          <feFlood floodColor="#FAA500" floodOpacity={0.3} result="color" />
          <feComposite in="color" in2="blur" operator="in" result="glow" />
          <feMerge>
            <feMergeNode in="glow" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
        <style>{`
          @keyframes icon-mol-spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
          .icon-mol { animation: icon-mol-spin 14s linear infinite; }
          @media (prefers-reduced-motion: reduce) { .icon-mol { animation: none; } }
        `}</style>
      </defs>
      <Molecule idp="icon" originX={53} originY={59} />
    </svg>
  );
}
