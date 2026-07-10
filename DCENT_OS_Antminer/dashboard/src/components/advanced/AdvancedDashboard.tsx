import React, { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { useMinerStore } from '../../store/miner';
import { api } from '../../api/client';
import { AuthGate } from '../common/AuthGate';
import { ModeSwitch } from '../common/ModeSwitch';
import { Console } from './Console';
import { AnomalyPanel } from './AnomalyPanel';
import { StatusStrip } from './StatusStrip';
import { CommandBar } from './CommandBar';
import { NotificationCenter, useNotificationDrawer } from './NotificationCenter';
import { RegisterInspector } from './RegisterInspector';
import { I2cScanner } from './I2cScanner';
import { AsicCommander } from './AsicCommander';
import { PidTuner } from './PidTuner';
import { ChipFreqMap } from './ChipFreqMap';
import { VoltageControl } from './VoltageControl';
import { DiagnosticsPanel } from './DiagnosticsPanel';
import { ExperimentalFlags } from './ExperimentalFlags';
import { MaintenanceMode } from './MaintenanceMode';
import { ApiExplorer } from './ApiExplorer';
import { Sv2ProtocolInspector } from './Sv2ProtocolInspector';
import { JobDeclarationPanel } from './JobDeclarationPanel';
import { PsuLab } from './PsuLab';
import { BeatLab } from './BeatLab';
import { UartFifoInspector } from './UartFifoInspector';
import { FlightRecorderPanel } from './FlightRecorderPanel';
import { ProtocolTimeline } from './ProtocolTimeline';
import { PipelineScope } from './PipelineScope';
import { ThermalReplayPanel } from './ThermalReplayPanel';
import { SiliconFingerprintPanel } from './SiliconFingerprintPanel';
import { PatchBayPanel } from './PatchBayPanel';
import { MacroRecipesPanel } from './MacroRecipesPanel';
import { SessionShareExportPanel } from './SessionShareExportPanel';
import { CommandJournalPanel } from './CommandJournalPanel';
import { BlockerStatePanel } from './BlockerStatePanel';
import { SystemDebug } from './SystemDebug';
import { AuditLogPanel } from './AuditLogPanel';

import { CompanionDock } from '../common/CompanionCard';
import { Am3BbInfoCard } from '../common/Am3BbInfoCard';
import { FindMyMiner } from '../common/FindMyMiner';
import { ModePillSwitch } from '../common/ModePillSwitch';
import { DonatingIndicator } from '../common/DonatingIndicator';
import { LiveAsicVisual } from '../common/LiveAsicVisual';
import { CommandPalette } from '../common/CommandPalette';
import { OverlayDialog } from '../common/OverlayDialog';
import { HardwareDetectionState } from '../common/HardwareDetectionState';
import { InfoBanner } from '../common/InfoBanner';
import { Tooltip } from '../common/Tooltip';
import { DcentOsIcon } from '../common/DcentOsLogo';
import type { PaletteItem } from '../common/CommandPalette';
import { ActiveHardwareProvider } from '../../hooks/useActiveHardware';
import { ADVANCED_SHORTCUT_KEYS, useKeyboardShortcuts, SHORTCUT_REFERENCE } from '../../hooks/useKeyboardShortcuts';
import { useDashboardHealth } from '../../hooks/useDashboardHealth';
import { useDeviceCapability } from '../../hooks/useDeviceCapability';
import { useModeNavigation } from '../../hooks/useModeNavigation';
import { useOverlayA11y } from '../../hooks/useOverlayA11y';
import { getPrimaryPage } from '../../utils/router';
import { getLiveWallWatts } from '../../utils/power';
import { wattsToBtu } from '../../utils/thermal';
import {
  isHardwareToolVisible,
  type PlatformCapabilities,
  type PlatformTier,
} from '../../utils/platformCapabilities';
import type { OperatingMode } from '../../api/types';
import type { DeviceCapabilityDescriptor } from '../../api/generated/capability';
import {
  IconOverview, IconTerminal, IconChipMap, IconFpga, IconBus,
  IconAsic, IconVoltage, IconApi, IconDiagnostics, IconProtocol,
  IconExperiment, IconPickaxe, IconScheduler, IconExport,
  IconLock, IconDebug,
} from '../icons/HackerIcons';

interface NavItem {
  id: string;
  label: string;
  icon: React.ReactNode;
  section: 'instruments' | 'hardware' | 'lab' | 'ops';
  shortcut?: string;
  blurb?: string;
}

const NAV_ITEMS: NavItem[] = [
  // Instruments — the always-on observability bay
  { id: 'dashboard', label: 'Overview', icon: <IconOverview />, section: 'instruments', blurb: 'live anomalies + chain truth' },
  { id: 'console', label: 'Console', icon: <IconTerminal />, section: 'instruments', shortcut: ADVANCED_SHORTCUT_KEYS.console, blurb: 'daemon logs · run diagnostics' },
  { id: 'chipmap', label: 'Chip Map', icon: <IconChipMap />, section: 'instruments', shortcut: ADVANCED_SHORTCUT_KEYS.chipmap, blurb: 'per-chip health · hotspots' },
  { id: 'sv2', label: 'Protocol', icon: <IconProtocol />, section: 'instruments', shortcut: ADVANCED_SHORTCUT_KEYS.sv2, blurb: 'stratum · job templates · sv2' },
  { id: 'beatlab', label: 'Beat Lab', icon: <IconAsic />, section: 'instruments', blurb: 'turn ASIC nonces into music' },
  // Hardware — the bench
  { id: 'fpga', label: 'FPGA Regs', icon: <IconFpga />, section: 'hardware', shortcut: ADVANCED_SHORTCUT_KEYS.fpga, blurb: 'mmio register inspector' },
  { id: 'i2c', label: 'I2C Bus', icon: <IconBus />, section: 'hardware', shortcut: ADVANCED_SHORTCUT_KEYS.i2c, blurb: 'i2cdetect · pic · psu · eeprom' },
  { id: 'uart', label: 'UART FIFO', icon: <IconBus />, section: 'hardware', blurb: 'chain UART FIFO inspector' },
  { id: 'asic', label: 'ASIC Cmd', icon: <IconAsic />, section: 'hardware', shortcut: ADVANCED_SHORTCUT_KEYS.asic, blurb: 'raw 55 AA frame builder' },
  { id: 'voltage', label: 'Voltage / PID', icon: <IconVoltage />, section: 'hardware', shortcut: ADVANCED_SHORTCUT_KEYS.voltage, blurb: 'chip rail · PID tuner' },
  { id: 'psu', label: 'PSU Lab', icon: <IconVoltage />, section: 'hardware', blurb: 'apw watchdog · output gate' },
  // Lab — research telemetry + replay
  { id: 'pipeline', label: 'Pipeline Scope', icon: <IconPickaxe />, section: 'lab', blurb: 'job in → nonce out · per chain' },
  { id: 'timeline', label: 'Protocol Timeline', icon: <IconProtocol />, section: 'lab', blurb: 'stratum frames on a timeline' },
  { id: 'replay', label: 'Thermal Replay', icon: <IconScheduler />, section: 'lab', blurb: 'recorded thermal history' },
  { id: 'fingerprint', label: 'Silicon Fingerprint', icon: <IconChipMap />, section: 'lab', blurb: 'per-die efficiency curve' },
  { id: 'blocker', label: 'Blockers', icon: <IconLock />, section: 'lab', blurb: 'live mining-blocker state' },
  // Ops — tape, journals, macros, exports
  { id: 'flight', label: 'Flight Recorder', icon: <IconExport />, section: 'ops', blurb: 'every chain event, captured' },
  { id: 'journal', label: 'Command Journal', icon: <IconTerminal />, section: 'ops', blurb: 'every action you ran' },
  { id: 'macros', label: 'Macros', icon: <IconExperiment />, section: 'ops', blurb: 'recipe builder · auto-run' },
  { id: 'patchbay', label: 'Patchbay', icon: <IconBus />, section: 'ops', blurb: 'route events → toast · marker · pulse · beep · freeze' },
  { id: 'session', label: 'Session Export', icon: <IconExport />, section: 'ops', blurb: 'redacted bundle for support' },
  { id: 'audit', label: 'Audit Trail', icon: <IconLock />, section: 'ops', blurb: 'persistent redacted action log' },
  { id: 'debug', label: 'System Debug', icon: <IconDebug />, section: 'ops', blurb: 'raw daemon internals' },
  { id: 'api', label: 'API', icon: <IconApi />, section: 'ops', shortcut: ADVANCED_SHORTCUT_KEYS.api, blurb: 'REST endpoint explorer' },
  { id: 'diagnostics', label: 'Diagnostics', icon: <IconDiagnostics />, section: 'ops', shortcut: ADVANCED_SHORTCUT_KEYS.diagnostics, blurb: 'health · maintenance · flags' },
];

export const ADVANCED_TOOL_IDS = NAV_ITEMS.map(item => item.id);

const PAGE_META: Record<string, { title: string; description: string }> = {
  dashboard: {
    title: 'Hardware Overview',
    description: 'Chain state, chip outliers, and anything that smells wrong at the silicon level.',
  },
  console: {
    title: 'Console',
    description: 'Raw dcentrald output and one-shot diagnostics — no SSH required.',
  },
  chipmap: {
    title: 'Chip Map',
    description: 'Per-chip health, hotspots, and which die is dragging the chain down.',
  },
  sv2: {
    title: 'Protocol Inspector',
    description: 'Stratum frames on the wire — V1, V2, version-rolling, share submissions.',
  },
  fpga: {
    title: 'FPGA Registers',
    description: 'Devmem-level register peek/poke on Zynq bitstreams. Read first, write rarely.',
  },
  i2c: {
    title: 'I2C Bus',
    description: 'PIC voltage controllers, EEPROMs, LM75 sensors — every byte on bus 0/1.',
  },
  asic: {
    title: 'ASIC Commander',
    description: 'Hand-craft chip-level frames. 0x55 0xAA, CRC, watch the wire.',
  },
  voltage: {
    title: 'Voltage And PID',
    description: 'Rail targets and the thermal control loop. Bench-grade with safety interlocks.',
  },
  psu: {
    title: 'PSU Lab',
    description: 'APW watchdog, output gating, and PMBus voltage programming. Loud safety rails.',
  },
  api: {
    title: 'API Explorer',
    description: 'Hit any DCENT_OS REST endpoint, see the raw JSON, copy the curl.',
  },
  diagnostics: {
    title: 'Diagnostics',
    description: 'Self-tests, recovery flows, and experimental routes for unstuck miners.',
  },
  // Round 6 restoration — meta for tools that were dropped from PAGE_META
  // when the shell was rewritten. Their files always existed; only the
  // routing was lost.
  beatlab: {
    title: 'Beat Lab',
    description: 'Turn ASIC noise + nonces into music. Every miner is also an instrument.',
  },
  uart: {
    title: 'UART FIFO Inspector',
    description: 'Live chain UART FIFO state — TX/RX bytes, fill level, parser state.',
  },
  pipeline: {
    title: 'Pipeline Scope',
    description: 'Job in, midstate computed, work dispatched, nonce returned. Per chain, end-to-end.',
  },
  timeline: {
    title: 'Protocol Timeline',
    description: 'Every Stratum frame on the wire, plotted on a timeline. Skip-back, diff, export.',
  },
  replay: {
    title: 'Thermal Replay',
    description: 'Recorded thermal history — pick a window, scrub it like a tape.',
  },
  fingerprint: {
    title: 'Silicon Fingerprint',
    description: 'Per-die efficiency curve. Find the chip that\'s dragging the chain down.',
  },
  blocker: {
    title: 'Blocker State',
    description: 'Live mining-blocker state — what\'s stopping nonces, in plain English.',
  },
  flight: {
    title: 'Flight Recorder',
    description: 'Every chain event captured to disk, ready to replay or export for review.',
  },
  journal: {
    title: 'Command Journal',
    description: 'Every action you ran, who triggered it, what the daemon said back.',
  },
  macros: {
    title: 'Macros',
    description: 'Recipe builder. Compose multi-step probes, run on demand or schedule.',
  },
  patchbay: {
    title: 'Patchbay',
    description: 'Route telemetry out — MQTT, webhook, syslog. Pick fields, set rate, go.',
  },
  session: {
    title: 'Session Export',
    description: 'Redacted browser-session bundle - selected state, logs, share history, and diagnostics.',
  },
  audit: {
    title: 'Audit Trail',
    description: 'Persistent, reboot-surviving log of operator actions. Newest first, secrets redacted.',
  },
  debug: {
    title: 'System Debug',
    description: 'Raw daemon internals — request IDs, queue depths, worker liveness.',
  },
};

// Descriptor-driven capability gate. The hook reads /api/v1/capabilities and
// falls back to system_info.platform_key only for older daemons that do not
// expose the shared descriptor endpoint.
interface PlatformGate {
  tier: PlatformTier;
  caps: PlatformCapabilities;
  descriptor: DeviceCapabilityDescriptor | null;
  loading: boolean;
  error: string | null;
}

function usePlatformGate(): PlatformGate {
  const systemInfo = useMinerStore(s => s.systemInfo);
  const capability = useDeviceCapability(systemInfo?.platform_key);
  return {
    tier: capability.tier,
    caps: capability.caps,
    descriptor: capability.descriptor,
    loading: capability.loading,
    error: capability.error,
  };
}

/** Filter NAV_ITEMS down to the tools visible for these capabilities. */
function visibleNavItems(caps: PlatformCapabilities): NavItem[] {
  return NAV_ITEMS.filter(item => isHardwareToolVisible(item.id, caps));
}

function hackerTabPath(pageId: string, tabId?: string) {
  return tabId ? `${pageId}/${tabId}` : pageId;
}

// BTU/h pill — shown in Hacker mode topbar
// (BTU/h must appear in ALL modes; every miner is a heater). Falls back to
// '---' when live wall-power telemetry is unavailable.
function BtuTopbarPill({ wallWatts }: { wallWatts: number | null | undefined }) {
  const watts = typeof wallWatts === 'number' && Number.isFinite(wallWatts) && wallWatts > 0
    ? wallWatts
    : null;
  const btu = watts !== null ? wattsToBtu(watts) : null;
  const display = btu !== null ? btu.toLocaleString() : '---';
  const title = watts !== null
    ? `${Math.round(watts).toLocaleString()} W → ${display} BTU/h (every miner is a heater)`
    : 'BTU/h not available — awaiting wall-watts telemetry';
  return (
    <Tooltip content={title} placement="bottom">
      <span
        className="ds-chip"
        data-testid="hacker-btu-pill"
        aria-label={title}
        role="status"
      >
        {display} BTU/h
      </span>
    </Tooltip>
  );
}

// Derive a stable DOM-safe id from a tab's path key (no colons, slashes, or
// special chars that would break querySelector / aria-controls).
function tabDomId(pageId: string, tabPagePath: string) {
  return `htab-${pageId}-${tabPagePath.replace(/\//g, '-')}`;
}
function panelDomId(pageId: string, tabPagePath: string) {
  return `hpanel-${pageId}-${tabPagePath.replace(/\//g, '-')}`;
}

function HackerTabbedPage({
  pageId,
  currentPage,
  onSelectPage,
  tabs,
}: {
  pageId: string;
  currentPage: string;
  onSelectPage: (page: string) => void;
  tabs: { id?: string; label: string; content: React.ReactNode }[];
}) {
  const activePath = currentPage === pageId || getPrimaryPage(currentPage) !== pageId
    ? pageId
    : currentPage;
  const activeTab = tabs.find(tab => hackerTabPath(pageId, tab.id) === activePath) ?? tabs[0];
  const activeTabPath = hackerTabPath(pageId, activeTab.id);

  return (
    <div className="hacker-tabbed-page">
      <div className="tab-bar hacker-inline-tab-bar" role="tablist">
        {tabs.map(tab => {
          const page = hackerTabPath(pageId, tab.id);
          const isActive = page === activeTabPath;
          const tId = tabDomId(pageId, page);
          const pId = panelDomId(pageId, page);

          return (
            <button
              key={page}
              id={tId}
              role="tab"
              aria-selected={isActive}
              aria-controls={pId}
              className={`tab ${isActive ? 'active' : ''}`}
              onClick={() => onSelectPage(page)}
            >
              {tab.label}
            </button>
          );
        })}
      </div>
      <div
        id={panelDomId(pageId, activeTabPath)}
        role="tabpanel"
        aria-labelledby={tabDomId(pageId, activeTabPath)}
        className="hacker-tabbed-content"
      >
        {activeTab.content}
      </div>
    </div>
  );
}

function AdvancedPage({
  page,
  setCurrentPage,
  caps,
  tier,
}: {
  page: string;
  setCurrentPage: (page: string) => void;
  caps: PlatformCapabilities;
  tier: PlatformTier;
}) {
  const primaryPage = getPrimaryPage(page);

  // A gated-out hardware tool must not be reachable even if `currentPage`
  // still points at it (e.g. a deep-link hash that survived a platform
  // change). Fall back to the overview rather than rendering a tool that
  // would shell platform-wrong hardware.
  const effectivePage = isHardwareToolVisible(primaryPage, caps)
    ? primaryPage
    : 'dashboard';

  const content = (() => {
    switch (effectivePage) {
      case 'console': return <Console />;
      case 'fpga': return <RegisterInspector />;
      case 'i2c': return <I2cScanner />;
      case 'uart': return <UartFifoInspector />;
      case 'asic': return <AsicCommander />;
      case 'chipmap': return <ChipFreqMap />;
      case 'beatlab': return <BeatLab />;
      case 'voltage':
        return <VoltagePidTabs currentPage={page} setCurrentPage={setCurrentPage} />;
      case 'psu':
        return <PsuLab />;
      case 'diagnostics':
        return <DiagnosticsTabs currentPage={page} setCurrentPage={setCurrentPage} />;
      case 'api': return <ApiExplorer />;
      case 'pipeline': return <PipelineScope />;
      case 'timeline': return <ProtocolTimeline />;
      case 'replay': return <ThermalReplayPanel />;
      case 'fingerprint': return <SiliconFingerprintPanel />;
      case 'blocker': return <BlockerStatePanel />;
      case 'flight': return <FlightRecorderPanel />;
      case 'journal': return <CommandJournalPanel />;
      case 'macros': return <MacroRecipesPanel />;
      case 'patchbay': return <PatchBayPanel />;
      case 'session': return <SessionShareExportPanel />;
      case 'audit': return <AuditLogPanel />;
      case 'debug': return <SystemDebug />;
      case 'sv2':
        return (
          <HackerTabbedPage
            pageId="sv2"
            currentPage={page}
            onSelectPage={setCurrentPage}
            tabs={[
              { label: 'Inspector', content: <Sv2ProtocolInspector /> },
              { id: 'templates', label: 'Own Templates', content: <JobDeclarationPanel /> },
            ]}
          />
        );
      default:
        return <AdvancedOverview setCurrentPage={setCurrentPage} caps={caps} tier={tier} />;
    }
  })();

  return content;
}

function VoltagePidTabs({ currentPage, setCurrentPage }: { currentPage: string; setCurrentPage: (page: string) => void }) {
  return (
    <HackerTabbedPage
      pageId="voltage"
      currentPage={currentPage}
      onSelectPage={setCurrentPage}
      tabs={[
        { label: 'Voltage Control', content: <VoltageControl /> },
        { id: 'pid', label: 'PID Tuner', content: <PidTuner /> },
      ]}
    />
  );
}

function DiagnosticsTabs({ currentPage, setCurrentPage }: { currentPage: string; setCurrentPage: (page: string) => void }) {
  return (
    <HackerTabbedPage
      pageId="diagnostics"
      currentPage={currentPage}
      onSelectPage={setCurrentPage}
      tabs={[
        { label: 'Diagnostics', content: <DiagnosticsPanel /> },
        { id: 'maintenance', label: 'Maintenance', content: <MaintenanceMode /> },
        { id: 'experiments', label: 'Experiments', content: <ExperimentalFlags /> },
      ]}
    />
  );
}

/**
 * Tool-launcher overview — laid out as a dense terminal admin grid à la
 * HackerMode.jsx. Each tile links to one of the deep tools above; the
 * AnomalyPanel + companion card sit at the top because they're load-bearing
 * "is anything on fire" signals.
 *
 *  Agent D: Adds a phone-mode disclaimer + tool search/filter input
 * to make the 32-tool grid usable on small viewports.
 */
function AdvancedOverview({
  setCurrentPage,
  caps,
  tier,
}: {
  setCurrentPage: (page: string) => void;
  caps: PlatformCapabilities;
  tier: PlatformTier;
}) {
  const status = useMinerStore(s => s.status);
  const systemInfo = useMinerStore(s => s.systemInfo);

  // Drop the overview tile itself, then hide any hardware tool that isn't
  // applicable to the running platform (fpga/i2c/uart/voltage/psu).
  const tools = visibleNavItems(caps).filter(n => n.id !== 'dashboard');

  // Phone-mode detection — non-blocking, dismissible per-render banner that
  // points narrow-viewport operators at Standard mode or landscape rotation.
  const [isPhone, setIsPhone] = useState(() =>
    typeof window !== 'undefined' && window.matchMedia
      ? window.matchMedia('(max-width: 767.98px)').matches
      : false
  );
  const [phoneNoticeDismissed, setPhoneNoticeDismissed] = useState(false);
  useEffect(() => {
    if (typeof window === 'undefined' || !window.matchMedia) return;
    const mql = window.matchMedia('(max-width: 767.98px)');
    const onChange = (e: MediaQueryListEvent) => setIsPhone(e.matches);
    // Older Safari/edge browsers expose addListener instead of addEventListener.
    if (mql.addEventListener) mql.addEventListener('change', onChange);
    else mql.addListener(onChange);
    return () => {
      if (mql.removeEventListener) mql.removeEventListener('change', onChange);
      else mql.removeListener(onChange);
    };
  }, []);

  // Tool search/filter — purely presentational; if the user types nothing,
  // the grid renders identically to the pre- state.
  const [toolQuery, setToolQuery] = useState('');
  const trimmedQuery = toolQuery.trim().toLowerCase();
  const filteredTools = trimmedQuery
    ? tools.filter(tool => {
        const haystack = `${tool.label} ${tool.blurb ?? ''} ${tool.id}`.toLowerCase();
        return haystack.includes(trimmedQuery);
      })
    : tools;

  return (
    <div className="hacker-overview">
      <LiveAsicVisual
        variant="hacker"
        compact
        title="ASIC Backplane"
        subtitle="Dense chain, chip, thermal, and nonce-adjacent context before opening a tool"
        actionLabel="Open chip map"
        onAction={() => setCurrentPage('chipmap')}
      />

      <AnomalyPanel />

      {/* am3-bb-only: AM335x BeagleBone Black port status. Gated by the
          fail-closed capability matrix so it only renders on that platform. */}
      {caps.bbSdCardRecovery && (
        <Am3BbInfoCard
          tier={tier}
          chainsDiscovered={status?.chains?.filter(ch => ch.chips > 0).length}
        />
      )}

      {isPhone && !phoneNoticeDismissed && (
        <InfoBanner
          tone="info"
          className="adv-infobanner-slot"
          dismissible
          onDismiss={() => setPhoneNoticeDismissed(true)}
        >
          Advanced mode is desktop-optimized — many tools require room to breathe. Consider switching to Standard mode for on-the-go use, or rotate to landscape.
        </InfoBanner>
      )}

      <section className="hacker-section">
        <header className="hacker-section-head">
          <span className="hacker-section-eyebrow">// instruments</span>
          <span className="hacker-section-meta">
            click a tile · {trimmedQuery
              ? `${filteredTools.length}/${tools.length} match`
              : `${tools.length} tools online`}
          </span>
        </header>
        <div className="adv-card-mb-12">
          <input
            type="search"
            className="ds-input"
            value={toolQuery}
            onChange={e => setToolQuery(e.target.value)}
            placeholder="Search advanced tools…"
            aria-label="Search advanced tools"
            autoComplete="off"
            spellCheck={false}
          />
        </div>
        {filteredTools.length === 0 ? (
          <div role="status" className="adv-empty-note">
            {`No tools match "${toolQuery.trim()}"`}
          </div>
        ) : (
          <div className="hacker-tool-grid">
            {filteredTools.map(tool => (
              <button
                key={tool.id}
                type="button"
                className="hacker-tool-tile"
                onClick={() => setCurrentPage(tool.id)}
                aria-label={tool.label}
              >
                <span className="hacker-tool-tile-icon">{tool.icon}</span>
                <span className="hacker-tool-tile-body">
                  <span className="hacker-tool-tile-label">
                    <span>{tool.label}</span>
                    {tool.shortcut && (
                      <span className="hacker-tool-tile-shortcut">{tool.shortcut}</span>
                    )}
                  </span>
                  {tool.blurb && (
                    <span className="hacker-tool-tile-blurb">{tool.blurb}</span>
                  )}
                </span>
                <span className="hacker-tool-tile-chevron" aria-hidden="true">›</span>
              </button>
            ))}
          </div>
        )}
      </section>

      <section className="hacker-section">
        <header className="hacker-section-head">
          <span className="hacker-section-eyebrow">// chain status</span>
          <span className="hacker-section-meta">
            {status?.chains?.length ?? 0} chains
          </span>
        </header>
        <div className="hacker-chain-grid">
          {(status?.chains ?? []).map(ch => {
            const alive = ch.chips > 0;
            return (
              <div key={ch.id} className={`hacker-chain-row ${alive ? 'is-alive' : 'is-dead'}`}>
                <span className="hacker-chain-id">chain{ch.id}</span>
                <span className="hacker-chain-data">
                  {alive
                    ? `${ch.chips} chips · ${ch.frequency_mhz} MHz · ${ch.temp_c.toFixed(1)}°C · ${ch.status}`
                    : 'no chips detected'}
                </span>
              </div>
            );
          })}
          {(status?.chains?.length ?? 0) === 0 && (
            <div className="hacker-chain-row is-dead">
              <span className="hacker-chain-id">chain0</span>
              <span className="hacker-chain-data">no telemetry</span>
            </div>
          )}
        </div>
      </section>

      <footer className="hacker-overview-footer">
        <span>
          {systemInfo?.chip_type ?? '---'} · {systemInfo?.model ?? ''} · {systemInfo?.board ?? ''} · v{status?.firmware_version ?? '?'}
        </span>
        <span className="hacker-overview-footer-hints">
          <kbd>Ctrl+K</kbd> palette · <kbd>?</kbd> help · <kbd>:</kbd> cmd · <kbd>Alt+1-9</kbd> nav
        </span>
      </footer>
    </div>
  );
}

function ShortcutHelpPanel({ onClose }: { onClose: () => void }) {
  const panelRef = useRef<HTMLDivElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    previousFocusRef.current = document.activeElement as HTMLElement | null;
    const timer = setTimeout(() => {
      panelRef.current?.focus();
    }, 0);

    return () => {
      clearTimeout(timer);
      previousFocusRef.current?.focus();
    };
  }, []);

  const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (e.key === 'Escape') {
      onClose();
      return;
    }
    if (e.key === 'Tab') {
      const focusable = panelRef.current?.querySelectorAll<HTMLElement>(
        'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
      );
      if (!focusable || focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (e.shiftKey) {
        if (document.activeElement === first || document.activeElement === panelRef.current) {
          e.preventDefault();
          last.focus();
        }
      } else {
        if (document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    }
  }, [onClose]);

  return (
    <OverlayDialog open onClose={onClose} ariaLabel="Keyboard shortcuts reference" maxWidth={420}>
      <div
        ref={panelRef}
        onKeyDown={handleKeyDown}
        className="adv-modal-pad"
      >
        <div className="adv-modal-title">
          Keyboard Shortcuts
        </div>
        <div className="adv-shortcut-list">
          {SHORTCUT_REFERENCE.map(s => (
            <div key={s.keys} className="adv-shortcut-row">
              <span className="adv-shortcut-keys">{s.keys}</span>
              <span className="adv-shortcut-desc">{s.description}</span>
            </div>
          ))}
        </div>
        <div className="adv-modal-foot">
          <button className="btn btn-secondary" onClick={onClose}>Close</button>
        </div>
      </div>
    </OverlayDialog>
  );
}

function AdvancedDashboardInner() {
  const currentPage = useMinerStore(s => s.currentPage);
  const setCurrentPage = useMinerStore(s => s.setCurrentPage);
  const collapsed = useMinerStore(s => s.sidebarCollapsed);
  const toggleSidebar = useMinerStore(s => s.toggleSidebar);
  const systemInfo = useMinerStore(s => s.systemInfo);
  const mode = useMinerStore(s => s.mode);
  const status = useMinerStore(s => s.status);
  const { switchMode } = useModeNavigation();
  const liveTopbarWallWatts = getLiveWallWatts(status?.power);

  // Per-platform capability gate (LANE A). `gatedNavItems` is the subset of
  // NAV_ITEMS that applies to the running platform; hardware tools that are
  // meaningless here (e.g. FPGA on Amlogic) are dropped from every nav surface.
  const {
    tier,
    caps,
    descriptor,
    loading: capabilityLoading,
    error: capabilityError,
  } = usePlatformGate();
  const gatedNavItems = useMemo(() => visibleNavItems(caps), [caps]);
  const isToolVisible = useCallback(
    (toolId: string) => isHardwareToolVisible(toolId, caps),
    [caps],
  );

  const [gateShown, setGateShown] = useState(() => sessionStorage.getItem('hacker-gate-dismissed') === '1');
  const [showPalette, setShowPalette] = useState(false);
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false);
  // Unified tool-search — filters BOTH the sidebar nav sections and the
  // horizontal tool tabbar (/4 carry-forward D-23: only the overview
  // grid had search before). Purely presentational; empty query renders
  // identically to the pre- state. Matches label / blurb / id.
  const [navQuery, setNavQuery] = useState('');
  const trimmedNavQuery = navQuery.trim().toLowerCase();
  const navMatches = useCallback((item: NavItem) => {
    if (!trimmedNavQuery) return true;
    return `${item.label} ${item.blurb ?? ''} ${item.id}`
      .toLowerCase()
      .includes(trimmedNavQuery);
  }, [trimmedNavQuery]);
  const notifDrawer = useNotificationDrawer();
  const { containerRef: mobileSidebarRef } = useOverlayA11y({
    open: mobileMenuOpen,
    onClose: () => setMobileMenuOpen(false),
  });

  const activePage = getPrimaryPage(currentPage);
  const pageMeta = PAGE_META[activePage] ?? PAGE_META.dashboard;
  const health = useDashboardHealth();
  const model = systemInfo?.model ?? 'Miner';
  const hostname = systemInfo?.hostname ?? 'Unnamed miner';
  const minerState = health.minerChip;
  const transportState = health.transportChip;
  const isMining = minerState.label === 'Mining';

  // Live clock for the console banner.
  const [clock, setClock] = useState(() => new Date());
  useEffect(() => {
    const id = window.setInterval(() => setClock(new Date()), 1000);
    return () => window.clearInterval(id);
  }, []);
  const hhmmss = clock.toTimeString().slice(0, 8);

  const { showHelp, setShowHelp } = useKeyboardShortcuts({
    setCurrentPage,
    onConsoleFocus: () => {
      setTimeout(() => {
        const input = document.querySelector('.console input[type="text"]:last-of-type') as HTMLInputElement;
        input?.focus();
      }, 100);
    },
    onCommandPalette: () => setShowPalette(true),
  });

  const paletteItems = useMemo((): PaletteItem[] => {
    const navItems: PaletteItem[] = gatedNavItems.map(item => ({
      id: `nav-${item.id}`,
      label: item.label,
      category: 'Navigate',
      action: () => setCurrentPage(item.id),
      shortcut: item.shortcut,
      description: PAGE_META[item.id]?.description ?? item.blurb,
      keywords: [item.id, item.label.toLowerCase()],
    }));

    // The PID Tuner lives under the Voltage & PID tool; if that tool is gated
    // out for this platform, its subtool entry must go too.
    const voltageSubtools: PaletteItem[] = isToolVisible('voltage')
      ? [{
          id: 'sub-voltage-pid',
          label: 'PID Tuner',
          category: 'Subtool',
          action: () => setCurrentPage('voltage/pid'),
          description: 'Open the PID tuning tab under Voltage & PID.',
          keywords: ['pid', 'fan', 'thermal', 'control loop'],
        }]
      : [];

    const subtools: PaletteItem[] = [
      ...voltageSubtools,
      {
        id: 'sub-diagnostics-maintenance',
        label: 'Maintenance Tools',
        category: 'Subtool',
        action: () => setCurrentPage('diagnostics/maintenance'),
        description: 'Open maintenance-mode diagnostics and service actions.',
        keywords: ['maintenance', 'repair', 'service'],
      },
      {
        id: 'sub-diagnostics-experiments',
        label: 'Experimental Flags',
        category: 'Subtool',
        action: () => setCurrentPage('diagnostics/experiments'),
        description: 'Open experimental and development-only feature toggles.',
        keywords: ['flags', 'experiments', 'dev'],
      },
    ];

    const actions: PaletteItem[] = [
      {
        id: 'action-restart',
        label: 'Restart Mining Daemon',
        category: 'Action',
        action: async () => { await api.restart(); },
        description: 'Restart dcentrald without leaving the dashboard.',
        keywords: ['restart', 'daemon', 'service'],
        dangerous: true,
        confirmDescription: 'Restart the mining daemon now?',
        successMessage: 'Mining daemon restart requested',
        errorMessage: 'Failed to restart mining daemon',
      },
      {
        id: 'action-find',
        label: 'Find My Miner',
        category: 'Action',
        action: async () => { await api.triggerLocate(); },
        description: 'Blink or identify this miner on the local network.',
        keywords: ['locate', 'blink', 'identify'],
        successMessage: 'Locate command sent',
        errorMessage: 'Failed to send locate command',
      },
      {
        id: 'action-standard',
        label: 'Switch to Standard Mode',
        category: 'Mode',
        action: () => { void switchMode('standard'); },
        description: 'Return to the standard operations dashboard.',
        keywords: ['standard', 'mining mode'],
      },
      {
        id: 'action-heater',
        label: 'Switch to Space Heater Mode',
        category: 'Mode',
        action: () => { void switchMode('heater'); },
        description: 'Switch to the simplified heater-first dashboard.',
        keywords: ['heater', 'simple mode'],
      },
    ];

    return [...navItems, ...subtools, ...actions];
  }, [setCurrentPage, switchMode, gatedNavItems, isToolVisible]);

  const version = systemInfo?.version || status?.firmware_version || '---';
  const ipDisplay = hostname;

  const renderNavSection = (sectionId: NavItem['section'], sectionLabel: string) => {
    const sectionItems = gatedNavItems.filter(i => i.section === sectionId && navMatches(i));
    // When a search is active and this section has no matches, drop the
    // section header too so the filtered nav stays tight.
    if (sectionItems.length === 0) return null;
    return (
    <>
      <div className="hacker-sidebar-section-label">
        {!collapsed && <span>{sectionLabel}</span>}
      </div>

      {sectionItems.map(item => (
        <button
          key={item.id}
          className={`nav-item ${activePage === item.id ? 'active' : ''}`}
          onClick={() => {
            setCurrentPage(item.id);
            setMobileMenuOpen(false);
          }}
          aria-current={activePage === item.id ? 'page' : undefined}
          aria-label={collapsed ? (item.shortcut ? `${item.label} (${item.shortcut})` : item.label) : undefined}
          title={item.shortcut ? `${item.label} (${item.shortcut})` : item.label}
        >
          <span className="nav-item-icon">{item.icon}</span>
          {!collapsed && (
            <span className="nav-item-body">
              <span>{item.label}</span>
              {item.shortcut && (
                <span className="nav-item-shortcut">{item.shortcut}</span>
              )}
            </span>
          )}
        </button>
      ))}
    </>
    );
  };

  return (
    <AuthGate>
      <div className="mode-hacker" data-testid="mode-hacker-dashboard">
        {mobileMenuOpen && (
          <button
            type="button"
            aria-label="Close advanced menu"
            className="hacker-mobile-overlay"
            onClick={() => setMobileMenuOpen(false)}
          />
        )}

        <aside
          ref={mobileSidebarRef}
          id="advanced-sidebar"
          className={`sidebar ${collapsed ? 'collapsed' : ''} ${mobileMenuOpen ? 'open' : ''}`}
          role={mobileMenuOpen ? 'dialog' : undefined}
          aria-modal={mobileMenuOpen ? true : undefined}
          aria-label={mobileMenuOpen ? 'Advanced navigation' : undefined}
          tabIndex={mobileMenuOpen ? -1 : undefined}
        >
          {mobileMenuOpen && (
            <button
              type="button"
              className="hacker-sidebar-mobile-close"
              aria-label="Close advanced menu"
              onClick={() => setMobileMenuOpen(false)}
            >
              ✕
            </button>
          )}
          <div className="brand hacker-sidebar-brand">
            {collapsed ? <DcentOsIcon size={24} /> : (
              <>
                <div className="hacker-sidebar-brand-row">
                  <DcentOsIcon size={22} />
                  <h1>DCENT_OS</h1>
                </div>
                <div className="version">
                  <span className="hacker-sidebar-prompt">root@dcentos:~#</span>
                  <span className="hacker-sidebar-version">v{version}</span>
                </div>
              </>
            )}
          </div>

          {!collapsed && (
            <div className="hacker-sidebar-search">
              <input
                type="search"
                value={navQuery}
                onChange={e => setNavQuery(e.target.value)}
                placeholder="Filter tools…"
                aria-label="Filter advanced tools in sidebar"
                autoComplete="off"
                spellCheck={false}
              />
            </div>
          )}

          <nav className="hacker-sidebar-nav">
            {renderNavSection('instruments', '// instruments')}
            {renderNavSection('hardware', '// hardware')}
            {renderNavSection('lab', '// lab')}
            {renderNavSection('ops', '// ops')}
            {!collapsed && trimmedNavQuery && !gatedNavItems.some(navMatches) && (
              <div className="hacker-sidebar-nav-empty" role="status">
                {`No tools match "${navQuery.trim()}"`}
              </div>
            )}
          </nav>

          <div className="hacker-sidebar-foot">
            {!collapsed && <CompanionDock />}
            {!collapsed && (
              <ModeSwitch currentMode={mode} onSelect={async (newMode: OperatingMode) => {
                void switchMode(newMode);
              }} compact />
            )}
            {collapsed && (
              <button
                className="nav-item"
                onClick={async () => {
                  const newMode = mode === 'hacker' ? 'standard' : mode === 'standard' ? 'heater' : 'hacker';
                  void switchMode(newMode);
                }}
                title={`Mode: ${mode} (click to cycle)`}
                aria-label={`Current mode: ${mode}. Click to cycle mode.`}
                style={{ justifyContent: 'center', marginBottom: 4 }}
              >
                <span className="nav-icon adv-mode-cycle-glyph">
                  {mode === 'heater' ? 'HTR' : mode === 'standard' ? 'STD' : 'ADV'}
                </span>
              </button>
            )}
            <button
              className="nav-item hacker-sidebar-collapse"
              onClick={toggleSidebar}
              aria-label={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
              aria-expanded={!collapsed}
            >
              <span>{collapsed ? '❯' : '❮'}</span>
              {!collapsed && <span>Collapse</span>}
            </button>
          </div>
        </aside>

        <div className={`main-content ${collapsed ? 'sidebar-collapsed' : ''}`} aria-hidden={mobileMenuOpen ? true : undefined}>
          <div className="warning-indicator" />

          {/* Console banner — terminal-style "model · IP · LED · uptime" */}
          <div className="hacker-banner">
            <button
              className="hacker-mobile-menu-btn"
              onClick={() => setMobileMenuOpen(!mobileMenuOpen)}
              aria-expanded={mobileMenuOpen}
              aria-controls="advanced-sidebar"
              aria-label={mobileMenuOpen ? 'Close advanced menu' : 'Open advanced menu'}
            >
              {'☰'}
            </button>
            <span className={`hacker-banner-led ${isMining ? 'is-on' : 'is-off'}`} aria-hidden="true" />
            <span className="hacker-banner-host">{model}</span>
            <span className="hacker-banner-sep">@</span>
            <span className="hacker-banner-ip">{ipDisplay}</span>
            <span className="hacker-banner-sep">·</span>
            <span className={`hacker-banner-state ${minerState.tone}`}>{minerState.label}</span>
            <span className="hacker-banner-sep">·</span>
            <span className={`hacker-banner-state ${transportState.tone}`}>{transportState.label}</span>
            <span className="hacker-banner-flex" />
            <BtuTopbarPill wallWatts={liveTopbarWallWatts > 0 ? liveTopbarWallWatts : null} />
            <DonatingIndicator />
            <ModePillSwitch />
            <span className={`hacker-banner-clock${isMining ? ' is-pulse' : ''}`}>{hhmmss}</span>
          </div>

          {/* Persistent context bar + inline-tabs / action chips.
              This is a page-context strip (heading + description + quick
              actions), NOT a navigation region. The action cluster gets a
              labelled group so screen readers announce it without implying
              landmark navigation. */}
          <div className="hacker-context-bar">
            <div className="hacker-context-meta">
              <h2 className="hacker-context-title">{pageMeta.title}</h2>
              <span className="hacker-context-copy">{pageMeta.description}</span>
            </div>
            <div
              className="hacker-context-actions"
              role="group"
              aria-label="Tool actions"
            >
              <button className="btn btn-secondary" onClick={() => setShowPalette(true)}>Palette</button>
              <button className="btn btn-secondary" onClick={() => setShowHelp(true)}>?</button>
              <FindMyMiner />
            </div>
          </div>

          {/* Persistent StatusStrip — always rendered, not just on overview */}
          <div className="hacker-status-strip-wrap">
            <StatusStrip />
          </div>

          <HardwareDetectionState
            descriptor={descriptor}
            loading={capabilityLoading}
            error={capabilityError}
          />

          {/* Horizontal tool tab-bar — quick inline switching across tools.
              Wave-5 D-23: the unified search lives in the sidebar (the same
              `navQuery` that drives both surfaces) and live-filters these tabs.
              Wave-9 ZONE-E: the duplicate tabbar search input was dropped — the
              sidebar filter is the single canonical entry; these tabs still
              react to `navQuery` so the two nav surfaces stay in lock-step. */}
          <div className="hacker-tool-tabbar" role="tablist" aria-label="Hacker tools">
            {gatedNavItems.filter(navMatches).map(item => {
              const isActive = activePage === item.id;
              return (
                <button
                  key={item.id}
                  role="tab"
                  aria-selected={isActive}
                  type="button"
                  className={`hacker-tool-tab ${isActive ? 'is-active' : ''}`}
                  onClick={() => setCurrentPage(item.id)}
                  title={item.shortcut ? `${item.label} (${item.shortcut})` : item.label}
                >
                  <span className="hacker-tool-tab-icon">{item.icon}</span>
                  <span className="hacker-tool-tab-label">{item.label}</span>
                </button>
              );
            })}
            {trimmedNavQuery && !gatedNavItems.some(navMatches) && (
              <span className="hacker-tool-tab is-search-empty" role="status">
                no match
              </span>
            )}
          </div>

          {/* Active page renders inside a padded scrollable region. We leave
              bottom padding for the sticky CommandBar. */}
          <div className="hacker-page-region">
            <AdvancedPage page={currentPage} setCurrentPage={setCurrentPage} caps={caps} tier={tier} />
          </div>
        </div>

        <CommandBar
          items={paletteItems}
          onOpenPalette={() => setShowPalette(true)}
          onShowHelp={() => setShowHelp(true)}
        />

        <NotificationCenter open={notifDrawer.open} onClose={() => notifDrawer.setOpen(false)} />

        {!gateShown && (
          <OverlayDialog open onClose={() => {}} ariaLabel="Advanced mode warning" dismissible={false} maxWidth={480}>
            <div className="adv-modal-danger ds-glass-strong">
              <div className="adv-modal-title is-danger">
                Advanced Mode
              </div>
              <p className="adv-modal-body">
                Direct hardware access is enabled. Incorrect register writes or voltage changes can permanently damage your miner.
              </p>
              <button
                className="btn btn-primary dcm-press"
                onClick={() => {
                  sessionStorage.setItem('hacker-gate-dismissed', '1');
                  setGateShown(true);
                }}
              >
                I understand the risks
              </button>
            </div>
          </OverlayDialog>
        )}

        {showHelp && <ShortcutHelpPanel onClose={() => setShowHelp(false)} />}

        <CommandPalette open={showPalette} onClose={() => setShowPalette(false)} items={paletteItems} />
      </div>
    </AuthGate>
  );
}

export function AdvancedDashboard() {
  return (
    <ActiveHardwareProvider>
      <AdvancedDashboardInner />
    </ActiveHardwareProvider>
  );
}
