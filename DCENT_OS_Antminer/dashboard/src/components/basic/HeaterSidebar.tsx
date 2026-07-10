import React from 'react';
import { useMinerStore } from '../../store/miner';
import { DcentOsIcon } from '../common/DcentOsLogo';
import { CompanionDock } from '../common/CompanionCard';

/**
 * Heater-mode LEFT sidebar — replaces the bottom-nav.
 *
 * The operator feedback was explicit: "the down menu instead of left … doesn't
 * feel natural for a miner." This mirrors the Standard-mode sidebar grammar
 * (brand → eyebrow → nav with orange active left-bar → status footer) so all
 * three modes share one navigation shape. On ≤768 px it collapses to a
 * horizontal top strip (CSS-driven). Nav state is the SAME store wiring the
 * old BottomNav used (`currentPage` / `setCurrentPage`) so per-mode nav memory
 * and task-handoff behaviour are unchanged.
 */
type HeaterPage = 'heater-home' | 'heater-history' | 'heater-settings';

const tabs: { id: HeaterPage; label: string; hint: string }[] = [
  { id: 'heater-home', label: 'Heater', hint: 'Live heat output, the thermostat dial, and one-tap power presets.' },
  { id: 'heater-history', label: 'History', hint: 'Heat delivered and sats earned over time, day by day.' },
  { id: 'heater-settings', label: 'Settings', hint: 'Quiet hours, electricity rate, safety limits, and earnings setup.' },
];

function FlameIcon({ active }: { active: boolean }) {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none"
      stroke={active ? 'var(--accent)' : 'var(--text-dim)'} strokeWidth="1.8"
      strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M8.5 14.5A2.5 2.5 0 0 0 11 12c0-1.38-.5-2-1-3-1.072-2.143-.224-4.054 2-6 .5 2.5 2 4.9 4 6.5 2 1.6 3 3.5 3 5.5a7 7 0 1 1-14 0c0-1.153.433-2.294 1-3a2.5 2.5 0 0 0 2.5 2.5z" />
    </svg>
  );
}
function ChartIcon({ active }: { active: boolean }) {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none"
      stroke={active ? 'var(--accent)' : 'var(--text-dim)'} strokeWidth="1.8"
      strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <line x1="18" y1="20" x2="18" y2="10" />
      <line x1="12" y1="20" x2="12" y2="4" />
      <line x1="6" y1="20" x2="6" y2="14" />
    </svg>
  );
}
function GearIcon({ active }: { active: boolean }) {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none"
      stroke={active ? 'var(--accent)' : 'var(--text-dim)'} strokeWidth="1.8"
      strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}

const iconMap: Record<HeaterPage, (active: boolean) => React.ReactNode> = {
  'heater-home': active => <FlameIcon active={active} />,
  'heater-history': active => <ChartIcon active={active} />,
  'heater-settings': active => <GearIcon active={active} />,
};

export function HeaterSidebar() {
  const currentPage = useMinerStore(s => s.currentPage);
  const setCurrentPage = useMinerStore(s => s.setCurrentPage);
  const systemInfo = useMinerStore(s => s.systemInfo);

  const HEATER_PAGES: readonly string[] = ['heater-home', 'heater-history', 'heater-settings'];
  const activePage = HEATER_PAGES.includes(currentPage) ? currentPage : 'heater-home';

  const host = systemInfo?.hostname || 'heater.local';
  const version = systemInfo?.version ?? '—';
  const model = systemInfo?.model ?? 'Miner';

  return (
    <aside className="basic-sidebar">
      <div className="basic-sidebar-brand">
        <DcentOsIcon size={28} />
        <div className="basic-sidebar-brand-text">
          <span className="basic-sidebar-brand-name">DCENT_OS</span>
          <span className="basic-sidebar-brand-tag">Heating</span>
        </div>
      </div>

      <div className="basic-sidebar-eyebrow">Operations</div>

      <nav className="basic-sidebar-nav" aria-label="Heater navigation">
        {tabs.map(tab => {
          const isActive = activePage === tab.id;
          return (
            <button
              key={tab.id}
              type="button"
              className={`basic-sidebar-item${isActive ? ' is-active' : ''}`}
              onClick={() => setCurrentPage(tab.id)}
              aria-current={isActive ? 'page' : undefined}
              data-tooltip={tab.hint}
              data-tooltip-pos="right"
            >
              <span className="basic-sidebar-bar" aria-hidden="true" />
              <span className="basic-sidebar-icon">{iconMap[tab.id](isActive)}</span>
              <span className="basic-sidebar-label">{tab.label}</span>
            </button>
          );
        })}
      </nav>

      <div className="basic-sidebar-spacer" />

      <CompanionDock />

      <div className="basic-sidebar-foot">
        <div className="basic-sidebar-foot-row">
          <span className="basic-sidebar-foot-dot" aria-hidden="true" />
          <span>{host}</span>
        </div>
        <div className="basic-sidebar-foot-row basic-sidebar-foot-mono">
          v{version} · {model}
        </div>
      </div>
    </aside>
  );
}
