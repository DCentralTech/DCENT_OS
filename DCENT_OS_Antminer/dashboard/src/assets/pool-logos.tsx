import React from 'react';

const logoStyle: React.CSSProperties = { display: 'inline-block', verticalAlign: 'middle' };

/* ─── Ocean ────────────────────────────────────────────── */
function OceanLogo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" style={logoStyle}>
      <path d="M1 7c2-2 4-2 6 0s4 2 6 0" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <path d="M1 10c2-2 4-2 6 0s4 2 6 0" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
    </svg>
  );
}

/* ─── Braiins ──────────────────────────────────────────── */
function BraiinsLogo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" style={logoStyle}>
      <rect x="2" y="3" width="5" height="5" rx="1" />
      <rect x="9" y="3" width="5" height="5" rx="1" />
      <rect x="5.5" y="8" width="5" height="5" rx="1" />
    </svg>
  );
}

/* ─── D-Central ────────────────────────────────────────── */
function DCentralLogo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" style={logoStyle}>
      <path d="M3 2h4a5 5 0 0 1 0 12H3V2zm2 2v8h2a3 3 0 0 0 0-6H5z" />
    </svg>
  );
}

/* ─── Foundry ──────────────────────────────────────────── */
function FoundryLogo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" style={logoStyle}>
      <path d="M8 1L2 5v6l6 4 6-4V5L8 1zm0 2.5L12 6v4l-4 2.5L4 10V6l4-2.5z" />
    </svg>
  );
}

/* ─── F2Pool ───────────────────────────────────────────── */
function F2PoolLogo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" style={logoStyle}>
      <path d="M4 3h6v2H6v2h3v2H6v4H4V3z" />
      <circle cx="12" cy="11" r="2.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  );
}

/* ─── AntPool ──────────────────────────────────────────── */
function AntPoolLogo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" style={logoStyle}>
      <circle cx="8" cy="6" r="3" />
      <ellipse cx="8" cy="11" rx="4" ry="2.5" />
      <line x1="5" y1="4" x2="3" y2="2" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" />
      <line x1="11" y1="4" x2="13" y2="2" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" />
    </svg>
  );
}

/* ─── ViaBTC ───────────────────────────────────────────── */
function ViaBTCLogo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" style={logoStyle}>
      <path d="M3 3l3 10h1.5L10 6l2.5 7H14L10 3H8.5l-2 6L4.5 3H3z" />
    </svg>
  );
}

/* ─── NiceHash ─────────────────────────────────────────── */
function NiceHashLogo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" style={logoStyle}>
      <path d="M4 3v10h2V7l4 6h2V3h-2v6L6 3H4z" />
    </svg>
  );
}

/* ─── Luxor ────────────────────────────────────────────── */
function LuxorLogo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" style={logoStyle}>
      <path d="M5 3v7h5v2H5h-1v-1V3h2z" />
      <circle cx="12" cy="5" r="2" fill="none" stroke="currentColor" strokeWidth="1.3" />
    </svg>
  );
}

/* ─── SoloCK ───────────────────────────────────────────── */
function SoloCKLogo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" style={logoStyle}>
      <circle cx="8" cy="8" r="5.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <circle cx="8" cy="8" r="2" />
    </svg>
  );
}

/* ─── PublicPool ────────────────────────────────────────── */
function PublicPoolLogo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" style={logoStyle}>
      <rect x="3" y="7" width="10" height="6" rx="1" fill="none" stroke="currentColor" strokeWidth="1.3" />
      <path d="M1 10h14" stroke="currentColor" strokeWidth="1" strokeDasharray="2 1" />
      <circle cx="8" cy="4" r="2" />
    </svg>
  );
}

/* ─── DEMAND ───────────────────────────────────────────── */
function DemandLogo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" style={logoStyle}>
      <path d="M2 3h5a5 5 0 0 1 0 10H2V3zm2 2v6h3a3 3 0 1 0 0-6H4z" />
      <rect x="12" y="5" width="2" height="6" rx="0.5" />
    </svg>
  );
}

/* ─── Kryptex ──────────────────────────────────────────── */
function KryptexLogo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" style={logoStyle}>
      <path d="M4 3v10h2V9l2 2 2-2v4h2V3h-2L8 6 6 3H4z" />
    </svg>
  );
}

/* ─── Hiveon ───────────────────────────────────────────── */
function HiveonLogo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" style={logoStyle}>
      <path d="M8 1L3 4.5v7L8 15l5-3.5v-7L8 1zm0 2.2l3 2.1v4.4l-3 2.1-3-2.1V5.3l3-2.1z" />
    </svg>
  );
}

/* ─── SpiderPool ───────────────────────────────────────── */
function SpiderPoolLogo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" style={logoStyle}>
      <circle cx="8" cy="8" r="2.5" fill="currentColor" stroke="none" />
      <line x1="8" y1="5.5" x2="5" y2="1" />
      <line x1="8" y1="5.5" x2="11" y2="1" />
      <line x1="5.5" y1="8" x2="1" y2="6" />
      <line x1="10.5" y1="8" x2="15" y2="6" />
      <line x1="5.8" y1="10" x2="2" y2="13" />
      <line x1="10.2" y1="10" x2="14" y2="13" />
      <line x1="8" y1="10.5" x2="6" y2="15" />
      <line x1="8" y1="10.5" x2="10" y2="15" />
    </svg>
  );
}

/* ─── Pool Logo Lookup ─────────────────────────────────── */
export const POOL_LOGOS: Record<string, React.FC<{ size?: number }>> = {
  'ocean': OceanLogo,
  'braiins': BraiinsLogo,
  'd-central': DCentralLogo,
  'foundry': FoundryLogo,
  'f2pool': F2PoolLogo,
  'antpool': AntPoolLogo,
  'viabtc': ViaBTCLogo,
  'nicehash': NiceHashLogo,
  'luxor': LuxorLogo,
  'ckpool': SoloCKLogo,
  'solo.ckpool': SoloCKLogo,
  'public-pool': PublicPoolLogo,
  'publicpool': PublicPoolLogo,
  'demand': DemandLogo,
  'kryptex': KryptexLogo,
  'hiveon': HiveonLogo,
  'spiderpool': SpiderPoolLogo,
};

/**
 * Look up a pool logo component from a stratum URL.
 * Returns null if no matching logo is found.
 */
export function getPoolLogo(poolUrl: string): React.FC<{ size?: number }> | null {
  const hostname = poolUrl
    .replace(/stratum\+tcp:\/\/|stratum2\+tcp:\/\/|stratum\+ssl:\/\//, '')
    .split(':')[0]
    .toLowerCase();
  for (const [key, Logo] of Object.entries(POOL_LOGOS)) {
    if (hostname.includes(key)) return Logo;
  }
  return null;
}
