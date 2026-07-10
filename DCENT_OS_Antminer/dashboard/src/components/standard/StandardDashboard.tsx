// StandardDashboard — STRUCTURAL recreation of the design-kit's
// `ui_kits/dashboard/index.html` App shell + `DashboardPage.jsx`
// composition, fed entirely by REAL store data.
//
// Kit App shell (exact target):
//   <div className="app">
//     <SidebarNav … />                 ← production Sidebar (kit-faithful)
//     <main className="main">
//       <TopBar onOpenConfig … />      ← KitTopBar
//       <div className="page-scroll">  ← kit page-scroll
//         {PageEl}                     ← KitDashboardPage | routed pages
//       </div>
//       <StatusFooter … />             ← OperatorStatusFooter (kit status-footer)
//     </main>
//   </div>
//
// The handoff skin (`src/styles/handoff-skin*.css`) maps production's
// `.mode-standard` / `.main-content` / `.standard-status-footer` onto the
// kit's `.app` / `.main` / `.status-footer` visual grammar — so the
// production wrapper class names are KEPT (the skin targets them) while the
// inner composition is rebuilt to the kit's DOM/component tree.
//
// EVERY page, the mode switch, the Advanced safety gate, tuning panels,
// status-footer truth chips, honest empty/loading states, data-testid and
// aria are preserved. Truth contracts are NOT waived: honest telemetry only,
// "connecting" ≠ "connected", zero hashrate ⇒ zero lit cells, no fabricated
// numbers. Look = kit; data = real.
import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { useMinerStore } from '../../store/miner';
import { formatHashrateShort, formatUptime } from '../../utils/format';
import { getPrimaryPage, STANDARD_SETTINGS_SUBPAGES } from '../../utils/router';
import { useDashboardHealth } from '../../hooks/useDashboardHealth';
import { useOverlayA11y } from '../../hooks/useOverlayA11y';
import { useValueFlash } from '../../hooks/useValueFlash';
import { useCountUp } from '../../hooks/useCountUp';
import { useTransportState } from '../../hooks/useTransportState';
import { AboutPage } from '../common/AboutPage';
import { getHonestModeState, HonestModeCard, useSystemHealth } from '../common/proxy/HonestModeStatus';
//  HIGH-2/3/4 (2026-05-24): `a lab unit`-class XIL bosminer-handoff surfaces.
// Both components self-gate on `a lab unit`-class hardware fingerprint — render
// nothing on every other unit (S9 / .109 / .79 / .135 / .129).
import { WaveRecipeBanner } from '../common/WaveRecipeBanner';
import { ChainPresencePanel } from '../common/ChainPresencePanel';
import { useSetupReadiness } from '../../hooks/useSetupReadiness';
import { useBetaView } from '../../store/miner';

import { computeMaxContinuousWatts } from '../wizard/CircuitConfigStep';

import { NAV_SECTIONS, Sidebar } from './Sidebar';
import { PoolConfig } from './PoolConfig';
import { TuningProfiles } from './TuningProfiles';
import { TuningPriorityConfigurator } from './TuningPriorityConfigurator';
import { FanCurveEditor } from './FanCurveEditor';
import { SiliconProfilesPanel } from './SiliconProfilesPanel';
import { PerChipOverridePanel } from './PerChipOverridePanel';
import { TempFansPage } from './TempFansPage';
import { LogsPage } from './LogsPage';
import { SettingsPage } from './SettingsPage';
import { EarningsPage } from './EarningsPage';
import { SharesPage } from './SharesPage';
import { Sv2StatusCard } from './Sv2StatusCard';
import { JobDeclarationPanel } from '../advanced/JobDeclarationPanel';
import { HardwareInfoPanel } from './HardwareInfo';
import { AutotunerCard } from './AutotunerCard';
import { AutotunerEvidencePanel } from './AutotunerEvidencePanel';
import { FleetView } from './FleetView';
import { EvidencePage } from './EvidencePage';

import {
  TouScheduler,
  GreenMiningPage,
  SolarConfig,
  CircuitCalculator,
  DemandResponse,
  FleetDiscovery,
  MqttConfig,
  DataExport,
  ProfileSharing,
  ImmersionMode,
  MethaneCalculator,
} from '../features';

import { OffGridPage } from '../features/OffGridPage';
import { ProfilesPage } from '../profile-import/ProfilesPage';
import { DangerZonePage } from '../restore-to-stock/DangerZonePage';
import { AutoTunerPage } from '../autotuner/AutoTunerPage';
import { Tooltip } from '../common/Tooltip';
import { glossaryText, type GlossaryKey } from '../../utils/glossary';
import { useFxPulse, useRewardFx } from '../../fx/useRewardFx';
import { FirstShareWatchCard } from '../common/FirstShareWatchCard';
import { CommandPalette } from '../common/CommandPalette';
import type { PaletteItem } from '../common/CommandPalette';
import { PageHeader, type PageHeaderAction, type PageHeaderStatus } from '../common/PageHeader';
import type { StatusPillState } from '../common/StatusPill';

import { KitTopBar } from './KitTopBar';
import { KitDeviceContext } from './KitDeviceContext';
import { KitDashboardPage } from './KitDashboardPage';

interface TabDefinition {
  id?: string;
  label: string;
  content: React.ReactNode;
}

const PAGE_META: Record<string, { title: string; description: string; infoKey: GlossaryKey }> = {
  dashboard: {
    title: 'Dashboard',
    description: 'Hashrate, share quality, and the next thing that needs your attention.',
    infoKey: 'hashrate_local_vs_pool',
  },
  pools: {
    title: 'Pools And Shares',
    description: 'Stratum endpoints, failover order, credentials, and share acceptance.',
    infoKey: 'pool_state',
  },
  earnings: {
    title: 'Profitability',
    description: 'Sats earned, watts spent, and what this run is actually worth.',
    infoKey: 'earning_proof',
  },
  temperature: {
    title: 'Thermals And Cooling',
    description: 'Per-board temps, fan PWM, PSU rail, and the safety envelope.',
    infoKey: 'temp_die_vs_board',
  },
  tuning: {
    title: 'Tuning',
    infoKey: 'autotuner_expectation',
    description: 'Frequency, voltage, autotuner state — heat vs. efficiency vs. quiet.',
  },
  logs: {
    title: 'Logs And Events',
    infoKey: 'telemetry_stale',
    description: 'Raw daemon output, warnings, and anything dcentrald wants you to see.',
  },
  evidence: {
    title: 'Proof And Catalog Evidence',
    infoKey: 'autotuner_receipts',
    description: 'Read-only diagnostics, hardware catalogs, and the provenance trail.',
  },
  settings: {
    title: 'Settings',
    infoKey: 'appearance_theme',
    description: 'Miner identity, network, alerts, and how this dashboard behaves locally.',
  },
  energy: {
    title: 'Energy Tools',
    infoKey: 'power_budget',
    description: 'Electricity rate plans, circuit limits, and power budgeting.',
  },
  integrations: {
    title: 'Integrations',
    infoKey: 'telemetry_live',
    description: 'Fleet tooling, MQTT, Home Assistant, exports, and profile sharing.',
  },
  fleet: {
    title: 'Fleet View',
    infoKey: 'swarm_os_fleet',
    description: 'Every miner you own, side by side — hashrate, thermals, reachability.',
  },
  offgrid: {
    title: 'Off-Grid',
    infoKey: 'power_budget',
    description: 'Solar, battery, intermittent power — DCENT_OS for unstable grids.',
  },
  profiles: {
    title: 'Silicon Profiles',
    infoKey: 'autotuner_receipts',
    description: 'Per (model × hashboard × chip) frequency and voltage tables. Diff, import, activate.',
  },
  system: {
    title: 'System',
    infoKey: 'restore_gates',
    description: 'Reboot, reflash, restore stock. Destructive — each step is explicit.',
  },
  autotuner: {
    title: 'Autotuner',
    infoKey: 'autotuner_convergence',
    description: 'Live tuning loop. Per-chain frequency, voltage, and silicon-binning targets.',
  },
};

const GLOSSARY_PALETTE_ROUTES = [
  { id: 'glossary-pool-state', label: 'Glossary / Pool status', page: 'pools', keywords: ['pool', 'connecting', 'authorized', 'mining'] },
  { id: 'glossary-shares', label: 'Glossary / Accepted shares', page: 'pools/shares', keywords: ['share', 'accepted', 'rejected', 'stale'] },
  { id: 'glossary-local-hashrate', label: 'Glossary / Local hashrate', page: 'dashboard', keywords: ['hashrate', 'local', 'pool'] },
  { id: 'glossary-wall-power', label: 'Glossary / Wall power', page: 'settings/general', keywords: ['power', 'watts', 'provenance'] },
  { id: 'glossary-danger-zone', label: 'System / Danger Zone', page: 'system', keywords: ['danger', 'restore', 'stock', 'reset'], dangerous: true },
] as const;

const PAGE_ACTION_ROUTES: Record<string, { label: string; page: string; tone?: PageHeaderAction['tone'] }> = {
  pools: { label: 'Shares', page: 'pools/shares' },
  tuning: { label: 'Autotuner', page: 'autotuner' },
};

function tabPath(pageId: string, tabId?: string) {
  return tabId ? `${pageId}/${tabId}` : pageId;
}

function TabbedPage({
  pageId,
  currentPage,
  onSelectPage,
  tabs,
}: {
  pageId: string;
  currentPage: string;
  onSelectPage: (page: string) => void;
  tabs: TabDefinition[];
}) {
  const activePath = currentPage === pageId || getPrimaryPage(currentPage) !== pageId
    ? pageId
    : currentPage;
  const activeTab = tabs.find(tab => tabPath(pageId, tab.id) === activePath) ?? tabs[0];

  return (
    <div className="standard-subtab-wrap">
      <div className="standard-subtab-bar">
        {tabs.map(tab => {
          const page = tabPath(pageId, tab.id);
          const isActive = page === tabPath(pageId, activeTab.id);

          return (
          <button
            key={page}
            type="button"
            className={`standard-subtab ${isActive ? 'active' : ''}`}
            onClick={() => onSelectPage(page)}
            aria-current={isActive ? 'page' : undefined}
          >
            {tab.label}
          </button>
          );
        })}
      </div>
      <div>{activeTab.content}</div>
    </div>
  );
}

function formatTelemetryAge(ageMs: number | null) {
  if (ageMs === null) return 'No samples';
  const seconds = Math.floor(ageMs / 1000);
  if (seconds < 5) return 'Just now';
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m ago`;
}

//  connection-state truth contract. The daemon now emits real pool
// states (connecting / authorized / mining / rejecting / disconnected /
// auth_failed) on PoolState.status. The footer pool chip must render each
// honestly — never a raw "auth_failed" token — and tint rejecting/auth_failed
// as warning/error rather than the previous fall-through to "Waiting".
function formatPoolStatus(poolStatus: string | undefined, hasConnection: boolean) {
  const normalized = (poolStatus ?? '').toLowerCase();
  if (normalized === 'mining') return 'Mining';
  if (normalized === 'connected' || normalized === 'active' || normalized === 'alive'
    || normalized === 'donating' || normalized === 'authorized' || normalized === 'mining_capable') {
    return 'Connected';
  }
  if (normalized === 'connecting' || normalized === 'configured') return 'Connecting';
  if (normalized === 'rejecting') return 'Rejecting';
  if (normalized === 'auth_failed') return 'Auth failed';
  if (normalized === 'disconnected' || normalized === 'dead') {
    return 'Disconnected';
  }
  return hasConnection ? 'Waiting' : 'Offline';
}

function getPoolTone(poolStatus: string | undefined, hasConnection: boolean) {
  const normalized = (poolStatus ?? '').toLowerCase();
  if (normalized === 'mining' || normalized === 'connected' || normalized === 'active'
    || normalized === 'alive' || normalized === 'donating' || normalized === 'authorized'
    || normalized === 'mining_capable') {
    return 'success';
  }
  if (normalized === 'auth_failed' || normalized === 'disconnected' || normalized === 'dead'
    || !hasConnection) {
    return 'danger';
  }
  // connecting / configured / rejecting / waiting all read as a non-fatal
  // attention state.
  return 'warning';
}

function toneToHeaderState(tone: string): StatusPillState {
  if (tone === 'success') return 'online';
  if (tone === 'danger') return 'error';
  if (tone === 'warning') return 'warning';
  return 'standby';
}

function autotunerHeaderStatus(
  status: { enabled?: boolean; phase?: string; state?: string; stale?: boolean } | null,
): PageHeaderStatus {
  if (!status) {
    return { state: 'telemetry_pending', label: 'Tuner status pending' };
  }
  if (!status.enabled) {
    return { state: 'standby', label: 'Tuner off' };
  }
  if (status.stale) {
    return { state: 'warning', label: 'Tuner stale' };
  }
  const phase = (status.phase || status.state || '').toLowerCase();
  if (phase === 'tuned' || phase === 'partially_tuned') {
    return { state: 'ready', label: 'Tuner stable' };
  }
  if (phase) {
    return { state: 'connecting', label: 'Tuner running', pulse: true };
  }
  return { state: 'ready', label: 'Tuner enabled' };
}

function getIssuePanelTone(level: string | undefined) {
  if (level === 'critical') return 'danger';
  if (level === 'warning') return 'warning';
  return 'info';
}

function OperatorStatusFooter({
  telemetry,
  miner,
  pool,
  hashboards,
  shares,
  uptime,
  onOpenLogs,
}: {
  telemetry: { value: string; note: string; tone: string };
  miner: { value: string; tone: string };
  pool: { value: string; note: string; tone: string };
  hashboards: { value: string; note: string; tone: string };
  shares: { value: string; note: string; tone: string };
  uptime: string;
  onOpenLogs: () => void;
}) {
  const [sharesRoll, pulseSharesRoll] = useFxPulse(620);
  const [poolRing, pulsePoolRing] = useFxPulse(820);

  useRewardFx(useCallback((event) => {
    if (event.intensity <= 0) return;
    if (event.kind === 'share-accepted' || event.kind === 'share-rejected') {
      pulseSharesRoll();
    } else if (event.kind === 'pool-transition') {
      pulsePoolRing();
    }
  }, [pulsePoolRing, pulseSharesRoll]));

  const items = [
    { testId: 'status-footer-telemetry', label: 'Telemetry', ...telemetry },
    { testId: 'status-footer-pool', label: 'Pool', ...pool },
    { testId: 'status-footer-hashboards', label: 'Hashboards', ...hashboards },
    { testId: 'status-footer-shares', label: 'Shares', ...shares },
  ];

  return (
    <footer className="standard-status-footer" data-testid="standard-status-footer" aria-label="Miner status summary">
      <button
        type="button"
        className="status-footer-logs-pill"
        data-testid="status-footer-logs"
        onClick={onOpenLogs}
      >
        Logs
      </button>
      <span className="sr-only" role="status" aria-live="polite">
        Miner {miner.value}; telemetry {telemetry.value}; pool {pool.value}
      </span>
      <div className="standard-status-footer-items">
        <span className={`standard-status-footer-chip ${miner.tone}`} data-testid="status-footer-miner">
          <span className="standard-status-footer-dot" aria-hidden="true" />
          Miner <strong>{miner.value}</strong>
        </span>
        {items.map(item => {
          // D-25: bare title= → F1 Tooltip. The runtime `note` is the
          // truthful per-state explanation; we also fold in the canonical
          // glossary explainer for Pool/Shares so the /9F honesty
          // contract is reinforced on hover.
          const glossKey =
            item.testId === 'status-footer-pool' ? 'pool_state' :
            item.testId === 'status-footer-shares' ? 'share_accepted' : null;
          const gloss = glossKey ? glossaryText(glossKey) : '';
          const tip = item.note
            ? (gloss ? `${item.note} — ${gloss}` : item.note)
            : gloss;
          const chipClass = [
            'standard-status-footer-chip',
            item.tone,
            item.testId === 'status-footer-pool' && poolRing ? 'dcfx-pool-ring' : '',
          ].filter(Boolean).join(' ');
          const valueClass = item.testId === 'status-footer-shares' && sharesRoll ? 'dcfx-footer-digit' : undefined;
          return (
            <Tooltip key={item.testId} content={tip || undefined} placement="top">
              <span className={chipClass} data-testid={item.testId}>
                {item.label} <strong className={valueClass}>{item.value}</strong>
              </span>
            </Tooltip>
          );
        })}
        <span className="standard-status-footer-chip neutral" data-testid="status-footer-uptime">
          Uptime <strong>{uptime}</strong>
        </span>
      </div>
    </footer>
  );
}

// W8.2 — read declared circuit config from the same wizard storage
// (`dcentos-wizard-state`) we wrote during onboarding. Returns nulls when
// the operator hasn't declared a circuit yet — CircuitWarning gates on these.
function readDeclaredCircuit(): { voltage: number | null; amperage: number | null; derate: number; capacityW: number | null } {
  try {
    const raw = localStorage.getItem('dcentos-wizard-state');
    if (!raw) return { voltage: null, amperage: null, derate: 0.8, capacityW: null };
    const parsed = JSON.parse(raw) as { circuitVoltage?: number | null; circuitAmperage?: number | null; circuitDerate?: number };
    const voltage = typeof parsed.circuitVoltage === 'number' ? parsed.circuitVoltage : null;
    const amperage = typeof parsed.circuitAmperage === 'number' ? parsed.circuitAmperage : null;
    const derate = typeof parsed.circuitDerate === 'number' ? parsed.circuitDerate : 0.8;
    const capacityW = computeMaxContinuousWatts({ voltage, amperage, derate });
    return { voltage, amperage, derate, capacityW };
  } catch {
    return { voltage: null, amperage: null, derate: 0.8, capacityW: null };
  }
}

export function StandardDashboard() {
  const currentPage = useMinerStore(s => s.currentPage);
  const setCurrentPage = useMinerStore(s => s.setCurrentPage);
  const sidebarCollapsed = useMinerStore(s => s.sidebarCollapsed);
  const systemInfo = useMinerStore(s => s.systemInfo);
  const status = useMinerStore(s => s.status);
  const autotunerStatus = useMinerStore(s => s.autotunerStatus);
  // Beta view = the calmer Standard-mode default. Power users can opt out
  // from Settings → General. See store/miner.ts::useBetaView.
  const betaView = useBetaView();
  void betaView;
  // W8.2 circuit config (read once per render — wizard rewrites localStorage
  // on every change so this stays fresh across pages).
  const circuitConfig = readDeclaredCircuit();

  const hashrateHistory = useMinerStore(s => s.hashrateHistory);
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false);
  const [showPalette, setShowPalette] = useState(false);
  const { containerRef: mobileSidebarRef } = useOverlayA11y({
    open: mobileMenuOpen,
    onClose: () => setMobileMenuOpen(false),
  });
  const primaryPage = getPrimaryPage(currentPage);
  const pageMeta = PAGE_META[primaryPage] ?? PAGE_META.dashboard;
  const health = useDashboardHealth();
  const readiness = useSetupReadiness('standard');
  const { health: systemHealth } = useSystemHealth();
  const honestState = getHonestModeState(systemHealth);
  const isProxyTelemetry = honestState === 'proxy_alive'
    || honestState === 'proxy_degraded'
    || honestState === 'hardware_blocked';

  // KPI values — REAL store data only.
  const hashrate = status?.hashrate_ghs ?? 0;
  const hr = formatHashrateShort(hashrate);
  const hashrateFlashCls = useValueFlash(hashrate);
  const uptimeFlashCls = useValueFlash(status?.uptime_s ?? null);
  const uptime = status?.uptime_s ?? systemInfo?.uptime_s ?? 0;
  const hrNumeric = parseFloat(hr.value);
  const hrCountUp = useCountUp(
    Number.isFinite(hrNumeric) ? hrNumeric : null,
    700,
    (n) => {
      const decimals = hr.value.includes('.') ? hr.value.split('.')[1].length : 0;
      return n.toFixed(decimals);
    },
  );
  const uptimeCountUp = useCountUp(
    uptime > 0 ? uptime : null,
    700,
    (n) => formatUptime(Math.max(0, Math.floor(n))),
  );
  const acceptedCountUp = useCountUp(
    status != null ? (status.accepted ?? 0) : null,
    700,
    (n) => Math.max(0, Math.round(n)).toLocaleString(),
  );
  const hasConnection = health.hasRecentTelemetry;
  const isMining = health.minerChip.label === 'Mining';
  const showHashrateKpi = isMining && (!isProxyTelemetry || honestState === 'proxy_alive');
  const topKpiLabel = showHashrateKpi
    ? isProxyTelemetry ? 'Proxied Hashrate' : 'Live Hashrate'
    : isProxyTelemetry ? 'Proxy State' : 'Status';
  const topKpiStatus = honestState === 'hardware_blocked'
    ? 'Blocked'
    : honestState === 'proxy_degraded'
      ? 'Reconnecting'
      : honestState === 'proxy_alive'
        ? 'Proxy alive'
        : hasConnection ? 'Standby' : 'Offline';

  const minerState = health.minerChip;
  const transportState = useTransportState();
  const primaryIssue = health.issues[0] ?? null;
  const poolDisplay = formatPoolStatus(status?.pool?.status, hasConnection);
  const poolTone = getPoolTone(status?.pool?.status, hasConnection);
  const poolHost = status?.pool?.url
    ? (status.pool.url.match(/:\/\/([^/]+)/)?.[1] ?? status.pool.url)
    : 'No pool URL reported';
  const chainCount = status?.chains?.length ?? 0;
  const activeChains = status?.chains?.filter(chain => chain.status?.toLowerCase() === 'active')?.length ?? 0;
  const chainsDisplay = !hasConnection
    ? 'Offline'
    : chainCount > 0
      ? `${activeChains}/${chainCount} active`
      : 'Waiting';
  const chainsTone = !hasConnection
    ? 'danger'
    : chainCount === 0
      ? 'warning'
      : activeChains === chainCount
        ? 'success'
        : 'warning';
  const acceptedShares = status?.accepted ?? 0;
  const rejectedShares = status?.rejected ?? 0;
  const shareTotal = acceptedShares + rejectedShares;
  const shareDisplay = !hasConnection
    ? 'Offline'
    : shareTotal > 0
      ? `${acceptedShares.toLocaleString()}/${rejectedShares.toLocaleString()}`
      : 'No shares';
  const shareTone = !hasConnection
    ? 'danger'
    : rejectedShares > 0
      ? 'warning'
      : acceptedShares > 0
        ? 'success'
        : 'neutral';

  const dashboardNextAction = readiness.primaryTask
    ? {
        label: readiness.primaryTask.label,
        detail: readiness.primaryTask.detail,
        actionLabel: readiness.primaryTask.actionLabel,
        onAction: readiness.primaryTask.onAction,
        tone: 'warning',
      }
    : primaryIssue
      ? {
          label: 'Resolve live issue',
          detail: primaryIssue.message,
          actionLabel: primaryIssue.key.includes('pool')
            ? 'Open Pools'
            : primaryIssue.key.includes('fan') || primaryIssue.key.includes('hot-chain') || primaryIssue.key.includes('chain-missing')
              ? 'Open Temp & Fans'
              : 'Open Logs',
          onAction: () => {
            if (primaryIssue.key.includes('pool')) {
              setCurrentPage('pools');
            } else if (primaryIssue.key.includes('fan') || primaryIssue.key.includes('hot-chain') || primaryIssue.key.includes('chain-missing')) {
              setCurrentPage('temperature');
            } else {
              setCurrentPage('logs');
            }
          },
          tone: getIssuePanelTone(primaryIssue.level),
        }
      : isMining
        ? {
            label: 'Check share quality',
            detail: 'Miner is hashing. Confirm accepted shares and pool health before changing tuning.',
            actionLabel: 'Open Pools & Shares',
            onAction: () => setCurrentPage('pools/shares'),
            tone: 'success',
          }
        : hasConnection
          ? {
              label: 'Start the mining path',
              detail: 'Miner telemetry is reachable. Verify pool credentials and work flow next.',
              actionLabel: 'Open Pool Setup',
              onAction: () => setCurrentPage('pools'),
              tone: 'info',
            }
          : {
              label: 'Reconnect telemetry',
              detail: 'Dashboard has no recent miner samples. Check runtime logs and network reachability.',
              actionLabel: 'Open Logs',
              onAction: () => setCurrentPage('logs'),
              tone: 'danger',
            };

  const footerUptime = uptime > 0 ? formatUptime(uptime) : hasConnection ? '< 1m' : 'Standby';

  const pageHeaderStatus: PageHeaderStatus = (() => {
    const minerHeaderStatus: PageHeaderStatus = {
      state: isMining ? 'mining' : hasConnection ? 'ready' : 'offline',
      label: minerState.label,
      pulse: isMining,
    };
    switch (primaryPage) {
      case 'dashboard':
        return minerHeaderStatus;
      case 'pools':
        return { state: toneToHeaderState(poolTone), label: poolDisplay, pulse: poolTone === 'success' };
      case 'tuning':
      case 'autotuner':
        return autotunerHeaderStatus(autotunerStatus);
      case 'logs':
      case 'integrations':
        return { state: toneToHeaderState(transportState.tone), label: transportState.label };
      case 'evidence':
        return { state: 'online', label: 'Read-only' };
      case 'settings':
        return readiness.primaryTask ? { state: 'warning', label: 'Setup pending' } : { state: 'ready', label: 'Configured' };
      case 'system':
        return { state: 'warning', label: 'Guarded' };
      default:
        return minerHeaderStatus;
    }
  })();

  const pageHeaderAction: PageHeaderAction | undefined = (() => {
    if (primaryPage === 'dashboard') {
      return {
        label: dashboardNextAction.actionLabel,
        onClick: dashboardNextAction.onAction,
        tone: dashboardNextAction.tone === 'danger' ? 'secondary' : 'primary',
      };
    }
    if (primaryPage === 'autotuner') {
      return { label: autotunerStatus?.enabled ? 'Review' : 'Enable', onClick: () => setCurrentPage('autotuner') };
    }
    const action = PAGE_ACTION_ROUTES[primaryPage];
    return action ? { ...action, onClick: () => setCurrentPage(action.page) } : undefined;
  })();

  useEffect(() => {
    const openPalette = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        setShowPalette(true);
      }
    };
    window.addEventListener('keydown', openPalette);
    return () => window.removeEventListener('keydown', openPalette);
  }, []);

  const paletteItems = useMemo<PaletteItem[]>(() => {
    const pageItems = NAV_SECTIONS.flatMap(section =>
      section.items.map(item => ({
        id: `standard-${item.id}`,
        label: item.label,
        category: section.header,
        description: PAGE_META[item.id]?.description ?? item.blurb,
        keywords: [item.id],
        dangerous: false,
        action: () => setCurrentPage(item.id),
      })),
    );
    const settingsItems = STANDARD_SETTINGS_SUBPAGES.map(item => ({
      id: `standard-${item.id}`,
      label: item.label,
      category: 'Settings',
      description: `Open ${item.label.toLowerCase()}.`,
      keywords: [...item.keywords],
      dangerous: false,
      action: () => setCurrentPage(item.id),
    }));
    const glossaryItems = GLOSSARY_PALETTE_ROUTES.map(item => {
      const dangerous = 'dangerous' in item && item.dangerous === true;
      return {
        id: item.id,
        label: item.label,
        category: dangerous ? 'Confirm' : 'Glossary',
        description: dangerous
          ? 'Open the gated system recovery page.'
          : `Open the page where ${item.label.replace('Glossary / ', '').toLowerCase()} is explained.`,
        keywords: [...item.keywords],
        dangerous,
        confirmDescription: dangerous
          ? 'Danger Zone contains restore-to-stock and other irreversible controls.'
          : undefined,
        action: () => setCurrentPage(item.page),
        successMessage: dangerous ? undefined : `Opened ${item.label.replace('Glossary / ', '')}`,
      };
    });
    return [...pageItems, ...settingsItems, ...glossaryItems];
  }, [setCurrentPage]);

  const renderPage = () => {
    switch (primaryPage) {
      case 'pools':
        return (
          <TabbedPage
            pageId="pools"
            currentPage={currentPage}
            onSelectPage={setCurrentPage}
            tabs={[
              { label: 'Pools', content: <PoolConfig /> },
              { id: 'shares', label: 'Shares', content: <SharesPage /> },
              { id: 'sv2', label: 'SV2 Status', content: <Sv2StatusCard /> },
              { id: 'own-templates', label: 'Own Templates', content: <JobDeclarationPanel /> },
          ]} />
        );
      case 'earnings':
        return <EarningsPage />;
      case 'temperature':
        return (
          <>
            {/* Wave-13: removed the standalone <FanMonitor /> that rendered
                here after TempFansPage — TempFansPage already contains the full
                fan-monitoring UI (gauges, RPM, FanControl), so it was a second
                complete duplicate fan section on the same page. */}
            <TempFansPage />
            <div className="temp-page-spacer">
              <HardwareInfoPanel />
              {/* Wave-55a HIGH-2: per-chain chips_responding/expected pill.
                  Self-gates on `a lab unit`-class XIL — returns null elsewhere. */}
              <ChainPresencePanel />
            </div>
            <div className="section temp-page-spacer">
              <ImmersionMode />
            </div>
          </>
        );
      case 'tuning':
        return (
          <div className="standard-tuning-surface standard-stagger">
            <AutotunerCard />
            <AutotunerEvidencePanel />
            <TuningPriorityConfigurator />
            <FanCurveEditor />
            <TuningProfiles />
            <SiliconProfilesPanel />
            <PerChipOverridePanel />
          </div>
        );
      case 'logs':
        return <LogsPage />;
      case 'evidence':
        return <EvidencePage />;
      case 'fleet':
        return <FleetView />;
      case 'offgrid':
        return <OffGridPage />;
      case 'profiles':
        return <ProfilesPage />;
      case 'autotuner':
        // W15-B: operator-facing autotuner control panel.
        return <AutoTunerPage />;
      case 'system':
        // Currently single sub-route: danger-zone.
        return <DangerZonePage />;
      case 'settings':
        return (
          <TabbedPage
            pageId="settings"
            currentPage={currentPage}
            onSelectPage={setCurrentPage}
            tabs={[
              { label: 'Settings', content: <SettingsPage /> },
              { id: 'about', label: 'About', content: <AboutPage /> },
          ]} />
        );
      case 'energy':
        return (
          <TabbedPage
            pageId="energy"
            currentPage={currentPage}
            onSelectPage={setCurrentPage}
            tabs={[
              { label: 'Rate Scheduler', content: <TouScheduler /> },
              { id: 'green', label: 'Green Mining', content: <GreenMiningPage /> },
              { id: 'solar', label: 'Solar Config', content: <SolarConfig /> },
              { id: 'circuit', label: 'Circuit Check', content: <CircuitCalculator /> },
              { id: 'demand', label: 'Demand Response', content: <DemandResponse /> },
              { id: 'methane', label: 'Methane Offset', content: <MethaneCalculator /> },
          ]} />
        );
      case 'integrations':
        return (
          <TabbedPage
            pageId="integrations"
            currentPage={currentPage}
            onSelectPage={setCurrentPage}
            tabs={[
              { label: 'Fleet Discovery', content: <FleetDiscovery /> },
              { id: 'mqtt', label: 'MQTT / HA', content: <MqttConfig /> },
              { id: 'export', label: 'Data Export', content: <DataExport /> },
              { id: 'profiles', label: 'Profiles', content: <ProfileSharing /> },
          ]} />
        );
      default:
        // Kit DashboardPage composition — real-data fed.
        return (
          <KitDashboardPage
            onOpenConfig={() => setCurrentPage('temperature')}
            nextAction={dashboardNextAction}
            circuit={{
              voltage: circuitConfig.voltage,
              amperage: circuitConfig.amperage,
              capacityW: circuitConfig.capacityW,
            }}
            onOpenPowerSettings={() => setCurrentPage('settings')}
          />
        );
    }
  };

  return (
    <div className="mode-standard app">
      {/* Mobile menu overlay */}
      {mobileMenuOpen && (
        <button
          type="button"
          aria-label="Close menu"
          className="standard-mobile-scrim"
          onClick={() => setMobileMenuOpen(false)}
          tabIndex={-1}
        />
      )}

      {/* Sidebar — kit `.sidebar`. Production Sidebar already mirrors the
          kit's SidebarNav.jsx structure (logo + grouped nav + mode switch +
          collapse footer) and is fully wired to every production route. */}
      <Sidebar
        sidebarId="standard-sidebar"
        sidebarRef={mobileSidebarRef as React.RefObject<HTMLElement>}
        mobileOpen={mobileMenuOpen}
        onNavigate={() => setMobileMenuOpen(false)}
      />

      {/* Kit `<main className="main">` — skin maps `.main-content`. */}
      <div className={`main main-content ${sidebarCollapsed ? 'sidebar-collapsed' : ''}`} aria-hidden={mobileMenuOpen ? true : undefined}>
        {/* Kit `.topbar` */}
        <KitTopBar
          pageTitle={pageMeta.title}
          pageDescription={pageMeta.description}
          minerState={minerState}
          hashrateValue={hrCountUp === '—' ? hr.value : hrCountUp}
          hashrateUnit={hr.unit}
          showHashrate={showHashrateKpi}
          mobileMenuOpen={mobileMenuOpen}
          onToggleMobileMenu={() => setMobileMenuOpen(!mobileMenuOpen)}
          onOpenSearch={() => setShowPalette(true)}
          onOpenConfig={() => setCurrentPage('temperature')}
          sidebarId="standard-sidebar"
        />
        <CommandPalette
          open={showPalette}
          onClose={() => setShowPalette(false)}
          items={paletteItems}
        />

        {/* Kit `.device-context` row — only on the Dashboard overview,
            exactly like DashboardPage.jsx where DeviceContext is the first
            child. Model + MAC/CB/PSU + 4 hero KPIs, all REAL data. */}
        {primaryPage === 'dashboard' && (
          <KitDeviceContext
            hashrateLabel={topKpiLabel}
            hashrateValue={hrCountUp === '—' ? hr.value : hrCountUp}
            hashrateUnit={hr.unit}
            showHashrate={showHashrateKpi}
            hashrateFlashClass={hashrateFlashCls}
            hashrateSpark={hashrateHistory.map(p => p.value)}
            isProxyTelemetry={isProxyTelemetry}
            topKpiStatus={topKpiStatus}
            uptimeValue={
              uptime > 0
                ? (uptimeCountUp === '—' ? formatUptime(uptime) : uptimeCountUp)
                : hasConnection ? '< 1m' : 'Standby'
            }
            uptimeFlashClass={uptimeFlashCls}
            poolDisplay={poolDisplay}
            poolHost={poolHost}
            poolLive={poolTone === 'success'}
            sharesValue={
              hasConnection && shareTotal > 0
                ? `${acceptedCountUp === '—' ? acceptedShares.toLocaleString() : acceptedCountUp}/${rejectedShares.toLocaleString()}`
                : shareDisplay
            }
            sharesSub={
              chainCount > 0
                ? `${activeChains}/${chainCount} chains · ${shareTotal > 0 ? 'live' : 'waiting'}`
                : 'no chains reported'
            }
          />
        )}

        {/* Kit `.page-scroll` */}
        <div className="page-scroll standard-page-scroll">
          <PageHeader
            title={pageMeta.title}
            description={pageMeta.description}
            status={pageHeaderStatus}
            primaryAction={pageHeaderAction}
            infoKey={pageMeta.infoKey}
          />
          {primaryPage !== 'dashboard' && !betaView && (
            <>
              <div className="ds-shell-offset">
                {/* Wave-55a HIGH-3/4: self-renders only on `a lab unit`-class XIL. */}
                <WaveRecipeBanner />
                <HonestModeCard compact />
              </div>
            </>
          )}

          {/* Page content — keyed so route changes re-mount and trigger the
              page-transition fade keyframe. */}
          <div key={primaryPage} className="page-transition-fadein">
            <FirstShareWatchCard />
            {renderPage()}
          </div>
        </div>

        {/* Kit `.status-footer` (skin maps `.standard-status-footer`). */}
        <OperatorStatusFooter
          telemetry={{ value: formatTelemetryAge(health.ageMs), note: transportState.label, tone: transportState.tone }}
          miner={{ value: minerState.label, tone: minerState.tone }}
          pool={{ value: poolDisplay, note: poolHost, tone: poolTone }}
          hashboards={{ value: chainsDisplay, note: chainCount > 0 ? `${chainCount} chain${chainCount === 1 ? '' : 's'} reported` : 'No chain rows yet', tone: chainsTone }}
          shares={{ value: shareDisplay, note: shareTotal > 0 ? 'accepted/rejected' : 'Waiting for accepted shares', tone: shareTone }}
          uptime={footerUptime}
          onOpenLogs={() => setCurrentPage('logs')}
        />
      </div>
    </div>
  );
}
