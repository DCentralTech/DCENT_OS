import React, { useEffect, useRef, useState } from 'react';
import { api } from '../../api/client';
import type { OperatingMode } from '../../api/types';
import { useModeNavigation } from '../../hooks/useModeNavigation';
import { useMinerStore } from '../../store/miner';
import { getSubPage } from '../../utils/router';
import { ActionButton } from '../common/ActionButton';
import { NextStepsPanel } from '../common/NextStepsPanel';
import { OverlayDialog } from '../common/OverlayDialog';
import { AppearanceTab } from './settings/AppearanceTab';
import { BackupTab } from './settings/BackupTab';
import { GeneralTab } from './settings/GeneralTab';
import { NetworkTab } from './settings/NetworkTab';
import { SecurityTab } from './settings/SecurityTab';

type SystemTab = 'general' | 'security' | 'network' | 'backup' | 'appearance';

const SYSTEM_TABS: Array<[SystemTab, string]> = [
  ['general', 'General'],
  ['security', 'Security'],
  ['network', 'Network'],
  ['backup', 'Backup & restore'],
  ['appearance', 'Appearance'],
];

function settingsTabFromPage(page: string): SystemTab | null {
  const subPage = getSubPage(page);
  return SYSTEM_TABS.some(([id]) => id === subPage) ? subPage as SystemTab : null;
}

function settingsPathForTab(tab: SystemTab): string {
  return `settings/${tab}`;
}

export function SettingsPage() {
  const systemInfo = useMinerStore(s => s.systemInfo);
  const settings = useMinerStore(s => s.settings);
  const currentPage = useMinerStore(s => s.currentPage);
  const setCurrentPage = useMinerStore(s => s.setCurrentPage);
  const addAlert = useMinerStore(s => s.addAlert);
  const { switchMode } = useModeNavigation();
  const [pendingMode, setPendingMode] = useState<OperatingMode | null>(null);
  const modeDialogRef = useRef<HTMLDivElement>(null);
  const modeCancelRef = useRef<HTMLButtonElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const [systemTab, setSystemTab] = useState<SystemTab>(() => {
    const tabFromRoute = settingsTabFromPage(currentPage);
    if (tabFromRoute) {
      return tabFromRoute;
    }
    try {
      const saved = localStorage.getItem('dcentos_system_tab');
      if (saved && SYSTEM_TABS.some(([id]) => id === saved)) {
        return saved as SystemTab;
      }
    } catch {
      /* localStorage unavailable */
    }
    return 'general';
  });

  useEffect(() => {
    const tabFromRoute = settingsTabFromPage(currentPage);
    if (tabFromRoute && tabFromRoute !== systemTab) {
      setSystemTab(tabFromRoute);
    }
  }, [currentPage, systemTab]);

  useEffect(() => {
    try {
      localStorage.setItem('dcentos_system_tab', systemTab);
    } catch {
      /* non-fatal */
    }
  }, [systemTab]);

  useEffect(() => {
    if (!pendingMode) {
      return;
    }

    previousFocusRef.current = document.activeElement as HTMLElement | null;
    const timer = setTimeout(() => {
      modeCancelRef.current?.focus();
    }, 0);

    return () => {
      clearTimeout(timer);
      previousFocusRef.current?.focus();
    };
  }, [pendingMode]);

  const selectTab = (tab: SystemTab) => {
    setSystemTab(tab);
    setCurrentPage(settingsPathForTab(tab));
  };

  const confirmModeSwitch = async () => {
    if (!pendingMode) return;
    const success = await switchMode(pendingMode);
    if (success) {
      addAlert('info', `Switched dashboard to ${pendingMode} mode.`);
    } else {
      addAlert('warning', 'Failed to switch mode');
    }
    setPendingMode(null);
  };

  const handleModeDialogKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.key === 'Escape') {
      setPendingMode(null);
      return;
    }
    if (e.key !== 'Tab') {
      return;
    }

    const focusable = modeDialogRef.current?.querySelectorAll<HTMLElement>(
      'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
    );
    if (!focusable || focusable.length === 0) {
      return;
    }

    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (e.shiftKey) {
      if (document.activeElement === first || document.activeElement === modeDialogRef.current) {
        e.preventDefault();
        last.focus();
      }
    } else if (document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };

  const renderTab = () => {
    switch (systemTab) {
      case 'security':
        return <SecurityTab />;
      case 'network':
        return <NetworkTab />;
      case 'backup':
        return <BackupTab />;
      case 'appearance':
        return <AppearanceTab />;
      case 'general':
      default:
        return <GeneralTab onModeSelect={setPendingMode} />;
    }
  };

  return (
    <div className="page-content">
      <div className="page-hero-strip">
        <div className="page-hero-summary">
          <div className="page-hero-eyebrow">PREFERENCES</div>
          <div className="page-hero-title">Settings</div>
          <div className="page-hero-substat">
            {settings.minerName ? `Configuring ${settings.minerName}` : 'Operating mode, donations, notifications, OTA, language, and miner identity.'}
          </div>
          <div className="page-hero-substat" style={{ marginTop: 4, opacity: 0.75 }}>
            {systemInfo?.version ? `Firmware ${systemInfo.version}` : 'Firmware version unavailable'}
            {systemInfo?.model ? ` · ${systemInfo.model}` : ''}
          </div>
        </div>
      </div>

      <NextStepsPanel mode="standard" />

      <div
        className="tab-underline"
        role="tablist"
        aria-label="System settings sections"
      >
        {SYSTEM_TABS.map(([id, label]) => (
          <button
            key={id}
            type="button"
            role="tab"
            aria-selected={systemTab === id}
            className={systemTab === id ? 'active' : ''}
            onClick={() => selectTab(id)}
          >
            {label}
          </button>
        ))}
      </div>

      {renderTab()}

      {pendingMode && (
        <OverlayDialog
          open={Boolean(pendingMode)}
          onClose={() => setPendingMode(null)}
          ariaLabel="Switch dashboard mode"
          initialFocusRef={modeCancelRef as React.RefObject<HTMLElement>}
          maxWidth={400}
        >
          <div ref={modeDialogRef} onKeyDown={handleModeDialogKeyDown} style={{ padding: 24 }}>
            <div style={{ fontWeight: 700, marginBottom: 12 }}>Switch Mode?</div>
            <div style={{ color: 'var(--text-secondary)', marginBottom: 20, fontSize: '0.9rem' }}>
              Switch the dashboard to <strong>{pendingMode}</strong> mode now?
              Your current page memory is preserved, and you can switch back anytime.
            </div>
            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
              <button ref={modeCancelRef} className="btn btn-secondary" onClick={() => setPendingMode(null)}>
                Cancel
              </button>
              <button className="btn btn-primary" onClick={confirmModeSwitch}>
                Switch Mode
              </button>
            </div>
          </div>
        </OverlayDialog>
      )}

      <div className="section">
        <div className="section-title">System Actions</div>
        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
          <ActionButton
            label="Restart Mining"
            variant="secondary"
            onClick={async () => { await api.restart(); }}
            confirm="Restart the mining daemon? Mining will be interrupted."
          />
          <ActionButton
            label="Reboot Miner"
            variant="danger"
            onClick={async () => { await api.reboot(); }}
            confirm="Reboot the entire miner? This takes about 60 seconds."
          />
        </div>
      </div>
    </div>
  );
}
