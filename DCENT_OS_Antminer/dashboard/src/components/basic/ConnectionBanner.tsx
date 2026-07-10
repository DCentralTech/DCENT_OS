import React, { useEffect, useState } from 'react';
import { useMinerStore } from '../../store/miner';
import { useDashboardHealth } from '../../hooks/useDashboardHealth';

export function ConnectionBanner() {
  const [showBanner, setShowBanner] = useState(false);
  const health = useDashboardHealth();

  // Show banner only after the degraded state has persisted briefly.
  useEffect(() => {
    if (health.hasFreshTelemetry) {
      setShowBanner(false);
      return;
    }

    const timer = setTimeout(() => {
      setShowBanner(true);
    }, 5000);

    return () => clearTimeout(timer);
  }, [health.hasFreshTelemetry, health.hasRecentTelemetry]);

  if (!showBanner) return null;

  const message = !health.hasRecentTelemetry
    ? 'Connection lost. Waiting for miner telemetry to return...'
    : 'Telemetry is stale. The dashboard is showing old data while it reconnects.';

  return (
    <div className="connection-banner" role="status" aria-live="polite">
      <svg
        className="connection-banner-icon ds-breathing-glow"
        width="16"
        height="16"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden="true"
      >
        <line x1="1" y1="1" x2="23" y2="23" />
        <path d="M16.72 11.06A10.94 10.94 0 0 1 19 12.55" />
        <path d="M5 12.55a10.94 10.94 0 0 1 5.17-2.39" />
        <path d="M10.71 5.05A16 16 0 0 1 22.56 9" />
        <path d="M1.42 9a15.91 15.91 0 0 1 4.7-2.88" />
        <path d="M8.53 16.11a6 6 0 0 1 6.95 0" />
        <line x1="12" y1="20" x2="12.01" y2="20" />
      </svg>
      <span>{message}</span>
    </div>
  );
}

/** Inline encrypted-connection indicator for pool status areas. */
export function EncryptedIndicator() {
  const status = useMinerStore(s => s.status);
  const encrypted = status?.pool?.encrypted;

  if (!encrypted) return null;

  return (
    <span className="encrypted-indicator">
      <svg
        width="12"
        height="12"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2.5"
        strokeLinecap="round"
        strokeLinejoin="round"
        className="encrypted-indicator-glyph"
      >
        {/* Lock body */}
        <rect x="3" y="11" width="18" height="11" rx="2" ry="2" />
        {/* Lock shackle */}
        <path d="M7 11V7a5 5 0 0 1 10 0v4" />
      </svg>
      <span>Encrypted</span>
    </span>
  );
}
