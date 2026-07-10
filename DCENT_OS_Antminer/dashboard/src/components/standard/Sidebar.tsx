import React, { useEffect, useState } from 'react';
import { useMinerStore } from '../../store/miner';
import { getPrimaryPage } from '../../utils/router';
import { useSetupReadiness } from '../../hooks/useSetupReadiness';
import { ModeSwitch } from '../common/ModeSwitch';
import { ReadinessCenter } from '../common/ReadinessCenter';
import { DcentOsIcon } from '../common/DcentOsLogo';
import { CompanionDock } from '../common/CompanionCard';
import { useModeNavigation } from '../../hooks/useModeNavigation';
import { Tooltip } from '../common/Tooltip';
import type { OperatingMode } from '../../api/types';

// ─── Inline SVG Icons (20x20, no library) ──────────────────
// Stroke geometry mirrors the inspiration kit (SidebarNav.jsx + Icons.jsx).
const icons: Record<string, React.ReactNode> = {
  dashboard: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <rect x="2" y="2" width="7" height="8" rx="1.5" />
      <rect x="11" y="2" width="7" height="5" rx="1.5" />
      <rect x="2" y="12" width="7" height="6" rx="1.5" />
      <rect x="11" y="9" width="7" height="9" rx="1.5" />
    </svg>
  ),
  pools: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="10" cy="10" r="7.5" />
      <path d="M10 5v5l3.5 3.5" />
      <circle cx="10" cy="10" r="1.2" fill="currentColor" />
    </svg>
  ),
  temperature: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <path d="M10 2v10" />
      <circle cx="10" cy="15" r="3" />
      <path d="M7 12V5a3 3 0 016 0v7" />
      <circle cx="10" cy="15" r="1.2" fill="currentColor" />
    </svg>
  ),
  tuning: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="10" cy="10" r="3" />
      <path d="M10 2v5M10 13v5M2 10h5M13 10h5" />
      <path d="M4.93 4.93l3.54 3.54M11.53 11.53l3.54 3.54M15.07 4.93l-3.54 3.54M8.47 11.53l-3.54 3.54" />
    </svg>
  ),
  earnings: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="10" cy="10" r="8" />
      <path d="M10 5v10M7.5 7.5h3.75a1.75 1.75 0 010 3.5H7.5M7.5 11h4a1.75 1.75 0 010 3.5H7.5" />
    </svg>
  ),
  shares: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <rect x="2" y="14" width="3" height="4" rx="0.5" />
      <rect x="6.5" y="10" width="3" height="8" rx="0.5" />
      <rect x="11" y="6" width="3" height="12" rx="0.5" />
      <rect x="15.5" y="3" width="3" height="15" rx="0.5" />
    </svg>
  ),
  logs: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <rect x="3" y="2" width="14" height="16" rx="2" />
      <path d="M7 6h6M7 9.5h6M7 13h4" />
    </svg>
  ),
  evidence: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <path d="M10 2l6 3v5c0 3.8-2.4 6.4-6 8-3.6-1.6-6-4.2-6-8V5l6-3z" />
      <path d="M7.5 10l1.7 1.7 3.4-3.8" />
    </svg>
  ),
  settings: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="10" cy="10" r="2.5" />
      <path d="M10 2v2.5M10 15.5V18M2 10h2.5M15.5 10H18M4.2 4.2l1.8 1.8M14 14l1.8 1.8M4.2 15.8L6 14M14 6l1.8-1.8" />
    </svg>
  ),
  green: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <path d="M10 18V9" />
      <path d="M5 10c0-5 5-8 5-8s5 3 5 8c0 3-2.2 5-5 5s-5-2-5-5z" />
      <path d="M7.5 12c0-2.5 2.5-4 2.5-4s2.5 1.5 2.5 4" />
    </svg>
  ),
  demand: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <path d="M11 2L5 12h5l-1 6 6-10h-5l1-6z" />
    </svg>
  ),
  fleet: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="10" cy="10" r="2" />
      <circle cx="4" cy="5" r="1.5" />
      <circle cx="16" cy="5" r="1.5" />
      <circle cx="4" cy="15" r="1.5" />
      <circle cx="16" cy="15" r="1.5" />
      <path d="M5.5 6l3 3M14.5 6l-3 3M5.5 14l3-3M14.5 14l-3-3" />
    </svg>
  ),
  profiles: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <rect x="3" y="3" width="14" height="4" rx="1" />
      <rect x="3" y="9" width="14" height="4" rx="1" />
      <rect x="3" y="15" width="8" height="3" rx="1" />
    </svg>
  ),
  danger: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <path d="M10 2L1.5 17h17L10 2z" />
      <path d="M10 8v4M10 14.5v0.01" />
    </svg>
  ),
  collapse: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 4l-5 6 5 6" />
    </svg>
  ),
  expand: (
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
      <path d="M8 4l5 6-5 6" />
    </svg>
  ),
};

interface NavSection {
  header: string;
  items: { id: string; label: string; icon: string; blurb: string }[];
}

// Kit-faithful LEAN sidebar grammar (ref: ui_kits/dashboard/SidebarNav.jsx).
// The kit's SidebarNav is a tight single-section nav: ONE `Operations`
// eyebrow + 6 primary items (Dashboard / Mining / Earnings / Shares /
// System / Logs), then the mode pill, then a mono footer. Production has
// ~15 routes — far more than the kit. We mirror the kit's structure WITHOUT
// dropping any page: a lead `Operations` section that tracks the kit's
// exact 6-concept set mapped onto production's real route IDs, then a
// secondary `More` section (still inside the kit's section-eyebrow grammar)
// that keeps every extra production page reachable.
//
// D-22: per-item `blurb` reaches Advanced-mode parity — every nav item
// has a plain-language hover explanation in both expanded AND collapsed
// states (collapsed previously had only a bare title=).
export const NAV_SECTIONS: NavSection[] = [
  {
    // Kit `Operations` eyebrow — the 6-item primary set, 1:1 with the
    // kit's Dashboard / Mining / Earnings / Shares / System / Logs.
    header: 'Operations',
    items: [
      { id: 'dashboard', label: 'Dashboard', icon: 'dashboard', blurb: 'Live overview of miner health, hashrate, and power.' },
      { id: 'tuning', label: 'Mining', icon: 'tuning', blurb: 'Pools, cooling, performance presets and advanced flags.' },
      { id: 'earnings', label: 'Earnings', icon: 'earnings', blurb: 'Sats earned, USD value, electricity cost, halving timeline.' },
      // NAV/IA contract UINAV-2/§3.3: `pools` is the Pool concept. `Shares`
      // is a SUB-VIEW of Pool (the in-page TabbedPage tab in
      // StandardDashboard's `pools` case), never a top-level alias.
      { id: 'pools', label: 'Pools', icon: 'pools', blurb: 'Pool config, plus accepted / rejected / stale shares (Pool-Target vs Achieved) as a sub-view.' },
      // UINAV-2/§3.3: the `settings` route is configuration — it must read
      // `Settings`, not `System` (the `system` route below carries that label).
      { id: 'settings', label: 'Settings', icon: 'settings', blurb: 'General, security, network, backup, appearance and firmware.' },
      { id: 'logs', label: 'Logs', icon: 'logs', blurb: 'Runtime messages, warnings and events.' },
    ],
  },
  {
    // Extra production pages the kit has no slot for — kept REACHABLE
    // under a secondary section eyebrow rather than deleted.
    header: 'More',
    items: [
      { id: 'fleet', label: 'Fleet View', icon: 'fleet', blurb: 'Every miner on one screen — per-unit hashrate, power, temperature and health at a glance.' },
      { id: 'temperature', label: 'Temp & Fans', icon: 'temperature', blurb: 'Die and board temperatures, fan PWM, and the cut-hash-before-noise safety posture.' },
      { id: 'autotuner', label: 'Autotuner', icon: 'tuning', blurb: 'The live optimizer — phase, convergence and per-chip health. Tuning takes time; that is expected.' },
      { id: 'energy', label: 'Energy Tools', icon: 'demand', blurb: 'Time-of-use scheduling, solar/green mining, demand response and circuit math.' },
      { id: 'offgrid', label: 'Off-Grid', icon: 'green', blurb: 'Battery-protective curtailment and direct-DC operation for off-grid setups.' },
      { id: 'integrations', label: 'Integrations', icon: 'fleet', blurb: 'pyasic / hass-miner / MQTT / data export — the stable API surface for third-party tools.' },
      { id: 'evidence', label: 'Evidence', icon: 'evidence', blurb: 'Receipts-only: read-only proof of state, backups and telemetry — never a derived guess.' },
      { id: 'profiles', label: 'Silicon Profiles', icon: 'profiles', blurb: 'Imported per-chip silicon profiles you can apply per hashboard.' },
      // UINAV-2/§3.3: the `system` route IS the System page. `Danger Zone`
      // is an in-page SUB-SECTION (the restore-to-stock / irreversible area),
      // not the whole page's name. Keep the warning icon so the gated
      // irreversible nature stays visible.
      { id: 'system', label: 'System', icon: 'danger', blurb: 'Device info and management, including the gated Danger Zone — restore-to-stock and other irreversible actions.' },
    ],
  },
];

// ─── DCENT_OS brand logo ──────────────────────────────────
// Canonical chip-package mark with the slowly-rotating D-Central
// particle cluster (DcentOsIcon, shared from common/), plus the
// styled "DCENT_OS" wordmark + blinking cursor.
function DcentOsMolecule({ collapsed }: { collapsed: boolean }) {
  return (
    <div className="sidebar-logo">
      <div className="sidebar-logo-chip" style={{ flexShrink: 0, filter: 'drop-shadow(0 0 10px rgba(250,103,0,.35))' }}>
        <DcentOsIcon size={32} />
      </div>
      {!collapsed && (
        <div className="sidebar-logo-wordmark">
          <span className="sidebar-logo-dcent">DCENT</span>
          <span className="sidebar-logo-underscore">_</span>
          <span className="sidebar-logo-os">OS</span>
          <span className="sidebar-logo-cursor" />
        </div>
      )}
    </div>
  );
}

interface SidebarProps {
  mobileOpen?: boolean;
  onNavigate?: () => void;
  sidebarId?: string;
  sidebarRef?: React.RefObject<HTMLElement>;
}

export function Sidebar({ mobileOpen, onNavigate, sidebarId, sidebarRef }: SidebarProps) {
  const currentPage = useMinerStore(s => s.currentPage);
  const setCurrentPage = useMinerStore(s => s.setCurrentPage);
  const collapsed = useMinerStore(s => s.sidebarCollapsed);
  const toggleSidebar = useMinerStore(s => s.toggleSidebar);
  const systemInfo = useMinerStore(s => s.systemInfo);
  const mode = useMinerStore(s => s.mode);
  const { switchMode } = useModeNavigation();

  const [readinessOpen, setReadinessOpen] = useState(false);
  const activePage = getPrimaryPage(currentPage);
  const readiness = useSetupReadiness('standard');
  const [isMobileViewport, setIsMobileViewport] = useState(false);

  useEffect(() => {
    const query = window.matchMedia('(max-width: 768px)');
    const update = () => setIsMobileViewport(query.matches);
    update();
    query.addEventListener('change', update);
    return () => query.removeEventListener('change', update);
  }, []);

  const isMobileDrawerOpen = isMobileViewport && Boolean(mobileOpen);
  const isMobileDrawerHidden = isMobileViewport && !mobileOpen;
  const visuallyCollapsed = collapsed && !isMobileDrawerOpen;

  useEffect(() => {
    const node = sidebarRef?.current;
    if (!node) return;
    if (isMobileDrawerHidden) {
      node.setAttribute('inert', '');
    } else {
      node.removeAttribute('inert');
    }
  }, [isMobileDrawerHidden, sidebarRef]);

  const version = systemInfo?.version || '—';
  const build = (systemInfo as { build?: string } | null)?.build || '';

  return (
    <aside
      ref={sidebarRef as React.RefObject<HTMLElement>}
      id={sidebarId}
      className={`sidebar ${visuallyCollapsed ? 'collapsed' : ''} ${mobileOpen ? 'open' : ''}`}
      role={isMobileDrawerOpen ? 'dialog' : 'navigation'}
      aria-modal={isMobileDrawerOpen ? true : undefined}
      aria-label={isMobileDrawerOpen ? 'Primary navigation' : 'Primary navigation'}
      aria-hidden={isMobileDrawerHidden ? true : undefined}
      tabIndex={isMobileDrawerOpen ? -1 : undefined}
    >
      <DcentOsMolecule collapsed={visuallyCollapsed} />

      {mobileOpen && (
        <button
          type="button"
          className="sidebar-mobile-close"
          onClick={onNavigate}
          aria-label="Close navigation menu"
        >
          ×
        </button>
      )}

      {readiness.showReadinessCta && (
        <div className="sidebar-readiness-slot">
          {!visuallyCollapsed ? (
            <button
              type="button"
              className="sidebar-readiness-cta"
              onClick={() => { setReadinessOpen(true); onNavigate?.(); }}
            >
              <div className="sidebar-readiness-cta-head">
                <span className="sidebar-readiness-cta-label">Readiness</span>
                <span className="sidebar-readiness-cta-count">{readiness.remainingTasks}</span>
              </div>
              <div className="sidebar-readiness-cta-title">{readiness.primaryTask?.label}</div>
              <div className="sidebar-readiness-cta-action">{readiness.primaryTask?.actionLabel}</div>
            </button>
          ) : (
            <Tooltip
              placement="right"
              content={`${readiness.remainingTasks} readiness task${readiness.remainingTasks === 1 ? '' : 's'} remaining`}
            >
              <button
                type="button"
                className="nav-item sidebar-readiness-compact"
                onClick={() => { setReadinessOpen(true); onNavigate?.(); }}
                aria-label={`${readiness.remainingTasks} readiness task${readiness.remainingTasks === 1 ? '' : 's'} remaining`}
              >
                <span className="sidebar-readiness-compact-label">R{readiness.remainingTasks}</span>
              </button>
            </Tooltip>
          )}
        </div>
      )}

      <nav className="sidebar-nav" aria-label="Dashboard sections">
        {NAV_SECTIONS.map(section => (
          <div key={section.header}>
            {!visuallyCollapsed ? (
              <div className="nav-section-header">
                <span>{section.header}</span>
              </div>
            ) : (
              <div className="nav-section-divider" aria-hidden="true" />
            )}
            {section.items.map(item => {
              const isActive = activePage === item.id;
              return (
                <Tooltip
                  key={item.id}
                  placement="right"
                  content={
                    visuallyCollapsed
                      ? <><b>{item.label}</b> — {item.blurb}</>
                      : item.blurb
                  }
                >
                  <button
                    type="button"
                    className={`nav-item ${isActive ? 'active' : ''}`}
                    onClick={() => { setCurrentPage(item.id); onNavigate?.(); }}
                    aria-current={isActive ? 'page' : undefined}
                    aria-label={visuallyCollapsed ? item.label : undefined}
                  >
                    <span className="nav-icon" aria-hidden="true">{icons[item.icon]}</span>
                    {!visuallyCollapsed && <span className="nav-label">{item.label}</span>}
                  </button>
                </Tooltip>
              );
            })}
          </div>
        ))}

        <div className="sidebar-mode-slot">
          {!visuallyCollapsed ? (
            <ModeSwitch
              currentMode={mode}
              onSelect={(newMode: OperatingMode) => { void switchMode(newMode); }}
              compact
            />
          ) : (
            <Tooltip placement="right" content={`Mode: ${mode} (click to cycle)`}>
              <button
                type="button"
                className="nav-item"
                onClick={() => {
                  void switchMode(mode === 'standard' ? 'hacker' : mode === 'hacker' ? 'heater' : 'standard');
                }}
                aria-label={`Current mode: ${mode}. Click to switch mode.`}
              >
                <span className="nav-icon sidebar-mode-compact-icon">
                  {mode === 'heater' ? 'HTR' : mode === 'standard' ? 'STD' : 'ADV'}
                </span>
              </button>
            </Tooltip>
          )}
        </div>
      </nav>

      <div className="sidebar-footer">
        {!visuallyCollapsed && <CompanionDock />}
        {!visuallyCollapsed && (
          <div className="sidebar-footer-text">
            Version
            <br />DCENT_OS {version}
            {build && (<><br />build {build}</>)}
          </div>
        )}
        <button
          type="button"
          className="icon-btn sidebar-collapse-btn"
          onClick={toggleSidebar}
          aria-label={visuallyCollapsed ? 'Expand sidebar' : 'Collapse sidebar'}
          aria-expanded={!visuallyCollapsed}
          data-tip={visuallyCollapsed ? 'Expand sidebar' : 'Collapse sidebar'}
        >
          {visuallyCollapsed ? icons.expand : icons.collapse}
        </button>
      </div>

      <ReadinessCenter open={readinessOpen} onClose={() => setReadinessOpen(false)} mode="standard" />
    </aside>
  );
}
