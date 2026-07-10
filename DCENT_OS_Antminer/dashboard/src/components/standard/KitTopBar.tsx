// KitTopBar — structural recreation of the design-kit's
// `ui_kits/dashboard/TopBar.jsx`, fed by REAL health/store data and with
// every production topbar capability preserved.
//
// Kit reference: ui_kits/dashboard/TopBar.jsx:
//   <div className="topbar">
//     <div className="topbar-left">
//        <span className="chip info live"><span className="dot"/>Mining</span>
//        HR pill (HR · value · TH/s)
//        <span className="chip neutral"><span className="dot"/>REST · 2s</span>
//     </div>
//     <div className="topbar-right">
//        Help desk · Documentation links · divider · clock · refresh · config icon
//     </div>
//   </div>
//
// Production additions kept (no capability lost): the mobile-menu button,
// the center page-context title/description, the DonatingIndicator chip,
// FindMyMiner, and the ModePillSwitch. The Mining/REST chips reflect REAL
// dashboard-health state (truth contract: "connecting" ≠ "connected").
import React, { useEffect, useState } from 'react';
import { DonatingIndicator } from '../common/DonatingIndicator';
import { FindMyMiner } from '../common/FindMyMiner';
import { ModePillSwitch } from '../common/ModePillSwitch';
import { TransportChip } from '../common/TransportChip';

function chipToneClass(tone: string): string {
  if (tone === 'success') return 'success';
  if (tone === 'warning') return 'warning';
  if (tone === 'danger') return 'danger';
  return 'neutral';
}

// Self-contained UTC clock for the kit `.topbar-refresh` slot. The kit shows
// "HH:MM:SS UTC"; the `::before` rule in the skin paints the green pulse dot.
function TopbarClock() {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const id = window.setInterval(() => setNow(new Date()), 1000);
    return () => window.clearInterval(id);
  }, []);
  const stamp = now.toLocaleTimeString(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
    timeZone: 'UTC',
  });
  return <>{stamp} UTC</>;
}

export interface KitTopBarProps {
  pageTitle: string;
  pageDescription: string;
  /** REAL miner state chip (label + tone) from useDashboardHealth. */
  minerState: { label: string; tone: string };
  /** REAL formatted live hashrate (value + unit) for the HR pill. */
  hashrateValue: string;
  hashrateUnit: string;
  showHashrate: boolean;
  mobileMenuOpen: boolean;
  onToggleMobileMenu: () => void;
  onOpenSearch: () => void;
  onOpenConfig: () => void;
  sidebarId: string;
}

export function KitTopBar(props: KitTopBarProps) {
  const [spin, setSpin] = useState(false);
  const triggerRefresh = () => {
    setSpin(true);
    window.setTimeout(() => setSpin(false), 900);
  };

  return (
    <div className="topbar">
      <div className="topbar-left">
        <button
          type="button"
          onClick={props.onToggleMobileMenu}
          aria-expanded={props.mobileMenuOpen}
          aria-controls={props.sidebarId}
          aria-label={props.mobileMenuOpen ? 'Close navigation menu' : 'Open navigation menu'}
          className="mobile-menu-btn icon-btn"
        >
          {/* Kit replicates icons as inline SVG (Icons.jsx) — no glyph/emoji. */}
          <svg width="16" height="16" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M3 5h14M3 10h14M3 15h14" />
          </svg>
        </button>
        <span
          className={`chip live ${chipToneClass(props.minerState.tone)}`}
          title={props.minerState.label}
        >
          <span className="dot" aria-hidden="true" />
          {props.minerState.label}
        </span>
        {props.showHashrate && (
          <div
            style={{
              display: 'inline-flex',
              alignItems: 'center',
              gap: 8,
              padding: '4px 12px',
              borderRadius: 999,
              background: 'rgba(10,10,15,.5)',
              border: '1px solid var(--border-glass)',
            }}
          >
            <span
              style={{
                fontSize: '.64rem',
                color: 'var(--fg-dim)',
                textTransform: 'uppercase',
                letterSpacing: '.12em',
                fontWeight: 700,
              }}
            >
              HR
            </span>
            <span
              style={{
                fontFamily: 'var(--font-heading)',
                fontWeight: 800,
                fontSize: '.98rem',
                color: 'var(--accent)',
                filter: 'drop-shadow(0 0 6px rgba(250,165,0,.35))',
                fontVariantNumeric: 'tabular-nums',
              }}
            >
              {props.hashrateValue}
            </span>
            <span style={{ fontSize: '.66rem', color: 'var(--fg-secondary)', fontWeight: 500 }}>
              {props.hashrateUnit}
            </span>
          </div>
        )}
        <TransportChip className="chip" dotClassName="dot" />
      </div>

      <div className="topbar-center">
        <div className="page-context-title">{props.pageTitle}</div>
        <div className="page-context-copy">{props.pageDescription}</div>
      </div>

      <div className="topbar-right">
        <button
          type="button"
          className="topbar-link"
          style={{ background: 'transparent', border: 0, padding: 0, font: 'inherit' }}
          onClick={props.onOpenSearch}
          aria-label="Search pages and glossary"
          data-tip="Search pages, settings, and glossary terms."
        >
          Search <kbd>Ctrl+K</kbd>
        </button>
        {/* Kit TopBar.jsx right cluster: Help desk + Documentation links,
            then a divider, the UTC clock, refresh, and config. The links
            point at the real DCENT_OS docs (open in a new tab). */}
        <a
          className="topbar-link"
          href="https://d-central.tech/contact/"
          target="_blank"
          rel="noopener noreferrer"
          data-tip="Open the D-Central help desk in a new tab."
        >
          Help desk
        </a>
        <a
          className="topbar-link"
          href="https://github.com/d-central-tech/dcentos"
          target="_blank"
          rel="noopener noreferrer"
          data-tip="Firmware reference, troubleshooting, install paths."
        >
          Documentation
        </a>
        <span className="topbar-divider" aria-hidden="true" />
        <span className="topbar-refresh" aria-label="Live telemetry">
          <TopbarClock />
        </span>
        <button
          className={`icon-btn ${spin ? 'spin' : ''}`}
          onClick={triggerRefresh}
          type="button"
          data-tip="Refresh miner status now"
          aria-label="Refresh miner status now"
        >
          <svg width="16" height="16" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
            <path d="M3 10a7 7 0 0112-5l2 2M17 4v4h-4" />
            <path d="M17 10a7 7 0 01-12 5l-2-2M3 16v-4h4" />
          </svg>
        </button>
        <button
          className="icon-btn"
          onClick={props.onOpenConfig}
          type="button"
          data-tip="Miner configuration (Power, Pools, General, ATM, Firmware)."
          aria-label="Open miner configuration"
        >
          <svg width="16" height="16" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="10" cy="10" r="2.5" />
            <path d="M10 2v2.5M10 15.5V18M2 10h2.5M15.5 10H18M4.2 4.2l1.8 1.8M14 14l1.8 1.8M4.2 15.8L6 14M14 6l1.8-1.8" />
          </svg>
        </button>
        {/* Production-only real actions (no kit slot) grouped after a
            divider so the kit's icon cluster reads cleanly first. */}
        <span className="topbar-divider" aria-hidden="true" />
        <DonatingIndicator />
        <FindMyMiner />
        <ModePillSwitch />
      </div>
    </div>
  );
}
