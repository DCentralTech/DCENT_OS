import React, { useState, useEffect, useRef } from 'react';
import { useMinerStore, useBetaView } from '../../store/miner';
import { api } from '../../api/client';
import { DashboardSkeleton } from '../common/Skeleton';
import { Thermostat } from './Thermostat';
import { PowerPresets } from './PowerPresets';
import { HeaterStatus } from './HeaterStatus';
import { NightModePill } from './NightModePill';
import { FindMyMiner } from '../common/FindMyMiner';
import { ModePillSwitch } from '../common/ModePillSwitch';
import { DcentOsIcon } from '../common/DcentOsLogo';
import { NextStepsPanel } from '../common/NextStepsPanel';
import { ReadinessCenter } from '../common/ReadinessCenter';
import { DonatingIndicator } from '../common/DonatingIndicator';
import { LiveAsicVisual } from '../common/LiveAsicVisual';
import { getHonestModeState, HonestModeCard, useSystemHealth } from '../common/proxy/HonestModeStatus';
import { ConnectionBanner } from './ConnectionBanner';
import { HeaterSidebar } from './HeaterSidebar';
import { HeaterBlockTicker } from './HeaterBlockTicker';
import { HeaterSensorSource } from './HeaterSensorSource';
import { HeaterBigReadouts } from './HeaterBigReadouts';
import { HeaterModeTiles } from './HeaterModeTiles';
import { HeaterEarningCard } from './HeaterEarningCard';
import { HeaterEnginePanel } from './HeaterEnginePanel';
import { HeaterEarningProof } from './HeaterEarningProof';
import { HistoryView } from './HistoryView';
import { SettingsView } from './SettingsView';
import { SafetyBadge } from './SafetyBadge';
import { getDisplayPowerWatts, getLiveDisplayWallWatts, getPowerTargetingLabel } from '../../utils/power';
import { useDashboardHealth } from '../../hooks/useDashboardHealth';
import { useSetupReadiness } from '../../hooks/useSetupReadiness';
import { Tooltip } from '../common/Tooltip';
import { InfoBanner } from '../common/InfoBanner';
import { TransportChip } from '../common/TransportChip';
import { glossaryText } from '../../utils/glossary';
import { FirstShareWatchCard } from '../common/FirstShareWatchCard';

const PAGE_META: Record<string, { title: string; description: string }> = {
  'heater-home': {
    title: 'Home Heat View',
    description: 'BTU output, mining state, and one tap to change how hard it works.',
  },
  'heater-history': {
    title: 'History',
    description: 'Heat delivered, sats earned, and how the room actually performed.',
  },
  'heater-settings': {
    title: 'Settings',
    description: 'Quiet hours, safety limits, and how the heater behaves day to day.',
  },
};

export function BasicDashboard() {
  const status = useMinerStore(s => s.status);
  const systemInfo = useMinerStore(s => s.systemInfo);
  const settings = useMinerStore(s => s.settings);
  const stats = useMinerStore(s => s.stats);
  const heaterStatus = useMinerStore(s => s.heaterStatus);
  const currentPage = useMinerStore(s => s.currentPage);
  const addToast = useMinerStore(s => s.addToast);
  const betaView = useBetaView();
  // Night-mode genuinely dims the shell when active. Toggle wiring stays in
  // NightMode.tsx / NightModePill.tsx — this consumes `nightMode.active` and
  // mirrors it onto the wrapper as `data-night-mode` so the CSS adapter
  // (basic.css `.mode-basic[data-night-mode='on']`) can apply a tasteful
  // brightness/saturation clamp without touching component logic or routing.
  const nightMode = useMinerStore(s => s.nightMode);
  const nightModeAttr = nightMode?.active ? 'on' : 'off';

  // Show skeleton until first data arrives
  const dataLoaded = status !== null && status !== undefined;
  const { health: systemHealth } = useSystemHealth();
  const honestState = getHonestModeState(systemHealth);
  const suppressLiveMiningClaim = honestState === 'proxy_degraded' || honestState === 'hardware_blocked';

  const health = useDashboardHealth();
  const isMining = status != null && status.hashrate_ghs > 0 && health.hasFreshTelemetry && !suppressLiveMiningClaim;
  const displayPowerWatts = getDisplayPowerWatts(heaterStatus, stats?.power);
  const liveDisplayPowerWatts = getLiveDisplayWallWatts(heaterStatus, stats?.power);
  const stateLinePower = liveDisplayPowerWatts > 0
    ? `${Math.round(liveDisplayPowerWatts)} W`
    : displayPowerWatts > 0
      ? `${Math.round(displayPowerWatts)} W est.`
      : '--- W';
  const powerTargetingLabel = getPowerTargetingLabel(stats?.power ?? heaterStatus);
  const powerControlSupported = systemInfo?.hardware?.capabilities?.sleep_wake_supported ?? false;
  const [toggling, setToggling] = useState(false);
  const [transitioning, setTransitioning] = useState<'starting' | 'stopping' | null>(null);
  const [readinessOpen, setReadinessOpen] = useState(false);
  const prevMiningRef = useRef(isMining);
  // Basic mode must NEVER render a blank void. `currentPage` is restored from
  // localStorage (per-mode nav memory) and can hold a stale non-heater value
  // (e.g. 'dashboard') left over from another mode — in which case none of the
  // heater-* page blocks below would match. Normalize anything unrecognized
  // back to the heater home view so the thermostat hero always renders.
  const HEATER_PAGES = ['heater-home', 'heater-history', 'heater-settings'] as const;
  const heaterPage = (HEATER_PAGES as readonly string[]).includes(currentPage)
    ? currentPage
    : 'heater-home';
  const pageMeta = PAGE_META[heaterPage] ?? PAGE_META['heater-home'];
  const model = systemInfo?.model ?? 'Miner';
  // Kit `nest-hero-eyebrow` = the zone name. Production has no "zone" concept;
  // the operator-facing equivalent is the miner's friendly name, which is the
  // honest, real label for "which thing am I heating with". No fabrication.
  const zoneLabel = settings.minerName || model;
  // Kit `nest-hero-status` headline — "Warming to target" / "At temperature".
  // Honest: only claims warming when telemetry confirms mining; otherwise the
  // standby line. No invented "cooling down" state (we have no setpoint-vs-room
  // delta we can prove without fabricating).
  const heroStatusLine = isMining ? 'Warming to target · earning sats' : 'Heater is in standby';
  const readiness = useSetupReadiness('heater');
  const showHonestModeCard = !betaView || honestState === 'proxy_degraded' || honestState === 'hardware_blocked';

  const minerState = honestState === 'proxy_alive'
    ? { label: 'Proxy active', tone: 'warning' as const }
    : honestState === 'proxy_degraded'
      ? { label: 'Proxy reconnecting', tone: 'warning' as const }
      : honestState === 'hardware_blocked'
        ? { label: 'Hardware blocked', tone: 'danger' as const }
        : (isMining
          ? { label: 'Heating', tone: 'success' as const }
          : health.minerChip.label === 'Mining'
            ? { label: 'Telemetry stale', tone: 'warning' as const }
          : health.minerChip);

  useEffect(() => {
    if (transitioning === 'starting' && isMining && !prevMiningRef.current) {
      setTransitioning(null);
    }
    if (transitioning === 'stopping' && !isMining && prevMiningRef.current) {
      setTransitioning(null);
    }
    prevMiningRef.current = isMining;
  }, [isMining, transitioning]);

  useEffect(() => {
    if (transitioning === 'stopping') {
      const timer = setTimeout(() => setTransitioning(null), 5000);
      return () => clearTimeout(timer);
    }
  }, [transitioning]);

  const handlePowerToggle = async () => {
    if (!powerControlSupported) {
      addToast('Start/stop control is in development for this hardware path.', 'warning');
      return;
    }

    setToggling(true);
    try {
      if (isMining) {
        await api.sleep();
        setTransitioning('stopping');
      } else {
        await api.wake();
        setTransitioning('starting');
      }
    } catch {
      addToast('Failed to change sleep/wake state', 'error');
      setTransitioning(null);
    } finally {
      setToggling(false);
    }
  };

  const powerControlNote = powerControlSupported
    ? 'Tap the center control to put the miner into standby or wake it back up.'
    : 'Dashboard start/stop is not available on this hardware path. Presets adjust heat targets only.';

  if (!dataLoaded) return <DashboardSkeleton />;

  // ─── Shared chrome (header + status strip + mode-switch + honest card) ───
  // Rendered once at the top of every heater page so navigation feels stable.
  const chrome = (
    <>
      {/* Connection banner */}
      <ConnectionBanner />

      {/* Header — kit `nest-header` (styles.css:1514): a frosted sticky bar
          with the D-Central brand molecule on the left and the BTC block
          pill + status on the right. Production keeps `dashboard-header`
          (the loaded skin pins it to the kit `.nest-header` wash) and adds
          `nest-header` so the coordinator's skin can address the kit class
          directly. Dual-class — the proven wizard pattern. */}
      <div className="dashboard-header nest-header">
        <div className="nest-brand">
          <div className="logo nest-brand-text">
            <DcentOsIcon size={26} />
            <span>DCENT_OS</span>
          </div>
          <div className="device-name nest-brand-tag">{settings.minerName} · {model}</div>
          <div className="basic-page-copy">{pageMeta.description}</div>
        </div>
        <div className="basic-header-actions nest-header-rh">
          <HeaterBlockTicker />
          {readiness.showReadinessCta && (
            <button
              type="button"
              className="basic-readiness-cta"
              onClick={() => setReadinessOpen(true)}
            >
              <span className="basic-readiness-cta__count">{readiness.remainingTasks}</span>
              <span>Readiness</span>
            </button>
          )}
          <DonatingIndicator />
          <FindMyMiner />
          <NightModePill />
        </div>
      </div>

      <div className="basic-status-strip">
        <Tooltip
          content={
            isMining
              ? 'Your heater is actively turning electricity into room heat (and Bitcoin work).'
              : honestState === 'hardware_blocked'
                ? glossaryText('honest_mode')
                : honestState === 'proxy_alive' || honestState === 'proxy_degraded'
                  ? glossaryText('hashrate_proxied')
                  : 'The heater is in standby. Tap the dial to start heating once a pool is configured.'
          }
        >
          <span className={`basic-status-chip ${minerState.tone}`}>{minerState.label}</span>
        </Tooltip>
        <Tooltip term="telemetry_live">
          <TransportChip className="basic-status-chip" showDot={false} />
        </Tooltip>
        <span className="basic-status-page">{pageMeta.title}</span>
      </div>

      {/*
        SafetyBadge — at-a-glance thermal-safety chip for heater-mode operators.
        Reads max chip temp from store.status.chains and surfaces normal / warm /
        hot. Renders in every heater page so the safety state is always visible
        next to the chrome strip. `.ds-shell-offset` replaces the old inline
        `padding:'0 20px 12px'` (D-28) — token-driven, shared with the honest
        card below so both shell insets stay identical (D-29).
      */}
      <div className="ds-shell-offset">
        <SafetyBadge />
      </div>

      {/*
        Home fan-cap expectation banner (R1 pain #3 — the #1 home complaint).
        Pure copy, pulled from the canonical glossary so it reinforces the
        cut-hash-before-noise truth-contract. Shows only while heating so it
        does not nag a standby unit.
      */}
      {isMining && (
        <InfoBanner
          tone="info"
          title="Cut power before noise"
          className="ds-shell-offset basic-quiet-boot-banner"
          dense
        >
          {glossaryText('quiet_boot')}
        </InfoBanner>
      )}

      <div className="basic-mode-switch-wrap">
        <ModePillSwitch />
      </div>

      {showHonestModeCard && (
        <div className="ds-shell-offset">
          <HonestModeCard compact />
        </div>
      )}
    </>
  );

  return (
    <div className="mode-basic" data-testid="mode-basic-dashboard" data-night-mode={nightModeAttr}>
      {/*
        Decorative ember field — only when mining is active. Lives behind every
        page (position: fixed, z-index: 0). Cypress-safe (no test ids, purely
        visual). Disabled when prefers-reduced-motion is set via the global
        guard in design-system.css.
      */}
      {isMining && (
        <div className="ds-ember-field" aria-hidden="true">
          <span className="ds-ember" />
          <span className="ds-ember lg" />
          <span className="ds-ember" />
          <span className="ds-ember" />
          <span className="ds-ember lg" />
          <span className="ds-ember" />
          <span className="ds-ember" />
          <span className="ds-ember lg" />
          <span className="ds-ember" />
          <span className="ds-ember" />
        </div>
      )}

      {/* Shell — kit `nest-shell` (styles.css:1499/2830): a sidebar + main
          composition. Production's `basic-shell`/`basic-main` are the same
          shape (flex sidebar + main); we dual-class with the kit names so
          the coordinator's skin can apply the kit grid/grad to the canonical
          `nest-*` selectors. HeaterSidebar (read-only reuse) is the kit
          `nest-sidebar` rail. */}
      <div className="basic-shell nest-shell">
        <HeaterSidebar />
        <div className="basic-main nest-main">
      {chrome}

      {/* ─── Heater home: kit 2-column `nest-hero-grid` ─────────────────────
          STRUCTURAL recreation of the kit composition (HeaterMode.jsx:706-757
          + styles.css:1560). A prior CSS skin failed because the composition
          differs: the kit hero is a 2-column grid (left = the ThermoDial
          column lead by an eyebrow + status headline, then the sensor switch,
          then the 3-up BigReadout row; right = a `nest-side` cards column),
          NOT a single centered stack. We now emit that DOM. Production class
          hooks are kept dual-classed so the loaded skin + responsive collapse
          + Cypress selectors all still resolve. */}
      {heaterPage === 'heater-home' && (
        <div key={heaterPage} className="heater-home-page nest-page nest-home page-transition-fadein">
          <div className="nest-hero-grid">
            {/* LEFT COLUMN — kit `nest-hero-dial` (styles.css:1568). Keeps the
                production `heater-page-hero` class so the pinned skin + the
                `::before` halo suppression still apply. */}
            <section className="heater-page-hero nest-hero-dial" aria-label="Heat hero">
              {/* Kit `nest-hero-eyebrow` (styles.css:1572) — the zone label.
                  Production's honest equivalent is the miner's friendly name. */}
              <div className="nest-hero-eyebrow">{zoneLabel}</div>
              {/* Kit `nest-hero-status` (styles.css:1576) — the warming/standby
                  headline above the dial. Honest: only "warming" when fresh
                  telemetry confirms mining. */}
              <div className="nest-hero-status">{heroStatusLine}</div>

              <Thermostat
                onToggle={powerControlSupported ? handlePowerToggle : undefined}
                isMining={isMining}
                toggling={toggling}
                powerControlSupported={powerControlSupported}
              />

              <div className="heater-hero-note">{powerControlNote}</div>

              {transitioning && (
                <div
                  className={`heater-transition-note ${transitioning === 'starting' ? 'is-starting' : 'is-stopping'}`}
                >
                  {transitioning === 'starting'
                    ? 'Waking up… controller outputs will return on the next safety tick'
                    : 'Entering standby… hash boards are powering down'}
                </div>
              )}

              <div className={`heater-state-line${isMining ? ' is-heating' : ''}`}>
                <span className="heater-state-dot" aria-hidden="true" />
                <span>
                  {isMining
                    ? `${heaterStatus?.preset ? heaterStatus.preset.charAt(0).toUpperCase() + heaterStatus.preset.slice(1) : 'Custom'} · ${stateLinePower} · ${powerTargetingLabel ?? 'Heating normally'}`
                    : 'Heater is off'}
                </span>
              </div>

              {/* Input-temperature source selector + the three at-a-glance
                  readouts (BTU/h · W · dB), placed directly under the dial —
                  exactly the kit's left-column order. */}
              <HeaterSensorSource />
              <HeaterBigReadouts />
            </section>

            {/* RIGHT COLUMN — kit `nest-side` cards (Phase-1 H2): mode tiles
                (Boost/Away/Quiet → real preset/night-mode actions), earning
                card (real sats/cost/net, honest empty), engine panel (real
                hashrate/chip-temp/draw + real LiveAsicVisual mini grid). */}
            <div className="nest-side" data-heater-side-slot>
              <HeaterModeTiles />
              <HeaterEarningCard />
              <HeaterEnginePanel />
              {/* "Are you actually earning?" verdict — placed at the foot of
                  the right column so it height-balances the tall dial column
                  and sits next to the earnings card it reassures about. */}
              <HeaterEarningProof />
            </div>
          </div>

          {/* heater-home does NOT render the full HeaterStatus — its BTU hero
              + cost/sats/noise/net cards would duplicate the hero's
              BigReadouts + EarningCard. The unique earning-proof verdict it
              used to own now lives in the hero right column above; the
              standalone duplicate LiveAsicVisual + SatsCounter were removed
              for the same de-duplication reason. (heater-history still renders
              the full HeaterStatus as that page's summary.) */}

          {/* Silicon telemetry — full width below the hero (moved out of the
              engine panel so it has room and the hero columns stay balanced).
              The ONLY ASIC grid on the page — no duplication. */}
          <FirstShareWatchCard />

          <LiveAsicVisual
            variant="heater"
            compact
            title="Heat Core"
            subtitle="Hashboards turning watts into useful room heat"
          />

          {/* Below the hero — power presets */}
          <section className="heater-home-presets" aria-label="Power presets">
            <PowerPresets />
          </section>

          {/* Commissioning checklist — below the heat hero, not above it */}
          <div className="heater-home-next-steps">
            <NextStepsPanel mode="heater" />
          </div>
        </div>
      )}

      {/* ─── Heater history ────────────────────────────────────────────────── */}
      {heaterPage === 'heater-history' && (
        <div key={heaterPage} className="heater-history-page nest-page page-transition-fadein">
          <HeaterStatus />
          <HistoryView />
        </div>
      )}

      {/* ─── Heater settings ───────────────────────────────────────────────── */}
      {heaterPage === 'heater-settings' && (
        <div key={heaterPage} className="heater-settings-page nest-page page-transition-fadein">
          <SettingsView />
        </div>
      )}

        </div>
      </div>

      <ReadinessCenter open={readinessOpen} onClose={() => setReadinessOpen(false)} mode="heater" />
    </div>
  );
}
