// KitDashboardPage — STRUCTURAL recreation of the design-kit's
// `ui_kits/dashboard/DashboardPage.jsx` composition, fed by REAL store data.
//
// Kit DashboardPage render tree (exact target):
//   <DeviceContext model mac cb psu />            ← KitDeviceContext
//   <div className="top-intel-grid">
//     <LiveAsicVisual title="ASIC Core" … />      ← production common/LiveAsicVisual (real)
//     <CurrentBlockCard compact/>                 ← production standard/CurrentBlockCard (real)
//   </div>
//   <AutotunerCard/>                              ← production standard/AutotunerCard (real)
//   <OverviewChart/>                              ← KitOverviewChart (real HashrateChart)
//   <div grid 1.1fr/2fr>
//     <FanMonitor/>                               ← KitFanMonitor (real fans)
//     <StatsKpiGrid/>                             ← KitStatsKpiGrid (real stats)
//   </div>
//   <HashBoardStrip/>                             ← KitHashBoardStrip (real chains)
//
// The DeviceContext lives in the topbar region (rendered by StandardDashboard,
// mirroring DashboardPage.jsx where DeviceContext is the first DashboardPage
// child but the kit-skin maps it under the topbar device-context row). This
// component owns everything BELOW the device-context row.
//
// Production truth/safety surfaces that the kit page has no slot for
// (CircuitWarning, the Operator-Focus panel, HonestModeCard) are preserved
// and folded into the same kit-styled `.section`/`.state-panel` flow so no
// capability or safety contract is lost. Loading state → DashboardSkeleton.
import React from 'react';
import { useMinerStore, useBetaView } from '../../store/miner';
import { selectIsMining } from '../../utils/miningStatus';
import { getLiveWallWatts } from '../../utils/power';
import { DashboardSkeleton } from '../common/Skeleton';
import { LiveAsicVisual } from '../common/LiveAsicVisual';
import { CircuitWarning } from '../heater/CircuitWarning';
import { getHonestModeState, HonestModeCard, useSystemHealth } from '../common/proxy/HonestModeStatus';

import { CurrentBlockCard } from './CurrentBlockCard';
import { AutotunerCard } from './AutotunerCard';
import { PlatformOverviewCard } from './PlatformOverviewCard';
import { CompetitiveReadinessCard } from './CompetitiveReadinessCard';
import { MiningPipelineManifestCard } from './MiningPipelineManifestCard';
import { NetworkContextCard } from './NetworkContextCard';
import { ThermalPowerPostureCard } from './ThermalPowerPostureCard';
import { MiningWorkPostureCard } from './MiningWorkPostureCard';

import { KitOverviewChart } from './KitOverviewChart';
import { KitFanMonitor } from './KitFanMonitor';
import { KitStatsKpiGrid } from './KitStatsKpiGrid';
import { KitHashBoardStrip } from './KitHashBoardStrip';

export interface KitDashboardPageProps {
  /** Open the hashboard / cooling config (kit: onOpenConfig). */
  onOpenConfig: () => void;
  /** Operator-Focus panel data (truth-driven next action). */
  nextAction: {
    label: string;
    detail: string;
    actionLabel: string;
    onAction: () => void;
    tone: string;
  };
  /** Declared circuit config for the safety CircuitWarning. */
  circuit: {
    voltage: number | null;
    amperage: number | null;
    capacityW: number | null;
  };
  onOpenPowerSettings: () => void;
}

export function KitDashboardPage(props: KitDashboardPageProps) {
  const status = useMinerStore(s => s.status);
  const betaView = useBetaView();
  const { health: systemHealth } = useSystemHealth();
  const honestState = getHonestModeState(systemHealth);

  // Loading: no first status sample yet → honest skeleton (no fabricated UI).
  if (status === null) {
    return <DashboardSkeleton />;
  }

  // ZONE-D : above-the-fold device-context health readout. Both values
  // are derived strictly from live telemetry and render "—" when the underlying
  // datum is unavailable — never fabricated. (Reject rate is led by
  // KitStatsKpiGrid below, so it is intentionally NOT duplicated in this strip.)
  const chains = status.chains ?? [];
  const reportedTemps = chains.map(c => c.temp_c).filter(t => typeof t === 'number' && Number.isFinite(t) && t > 0);
  const maxChipTemp = reportedTemps.length > 0 ? Math.max(...reportedTemps) : null;
  const chipsResponding = chains.reduce((sum, c) => sum + (c.chips ?? 0), 0);
  // Canonical whole-miner mining state (Omega P0-7 / C-8) — same selector the
  // topbar chip + favicon use, so the `is-mining` grid below can never disagree
  // with them off the same status sample.
  const isMining = selectIsMining(status);

  const tempTone = maxChipTemp == null ? '' : maxChipTemp >= 75 ? 'is-danger' : maxChipTemp >= 65 ? 'is-warn' : 'is-ok';
  const liveCircuitWatts = getLiveWallWatts(status?.power);

  return (
    <div className="page-content standard-dashboard-stack standard-stagger">
      {/* Safety: circuit overdraw warning (gated on declared circuit). */}
      <CircuitWarning
        currentWatts={liveCircuitWatts > 0 ? liveCircuitWatts : null}
        voltageV={props.circuit.voltage}
        amperageA={props.circuit.amperage}
        circuitCapacityW={props.circuit.capacityW}
        onOpenPowerSettings={props.onOpenPowerSettings}
      />

      {/* ZONE-D Wave-9: compact device-context health strip above the fold. */}
      <div className="kit-device-strip" aria-label="Device health summary">
        <div className="kit-device-tile">
          <span className="kit-device-tile-label">Max Chip Temp</span>
          <span className={`kit-device-tile-value ${tempTone}`}>
            {maxChipTemp == null ? '—' : `${maxChipTemp.toFixed(1)}°C`}
          </span>
        </div>
        <div className="kit-device-tile">
          <span className="kit-device-tile-label">Chips Responding</span>
          <span className="kit-device-tile-value">
            {chipsResponding > 0 ? chipsResponding.toLocaleString() : '—'}
          </span>
        </div>
      </div>

      {/* Kit: top intelligence grid — live silicon + Bitcoin network truth.
          Both are the REAL production components (store/API fed). The
          `standard-top-intelligence-grid` (WITHOUT `standard-command-core`)
          picks up the kit's exact 2-col `.top-intel-grid` spec from the
          handoff skin. ZONE-D adds `is-mining` when fresh telemetry shows
          hashing (the styling rule itself is ZONE-A's). */}
      <section
        className={`top-intel-grid standard-top-intelligence-grid${isMining ? ' is-mining' : ''}`}
        data-testid="standard-premium-top-card"
        aria-label="Mining core and Bitcoin block context"
      >
        <LiveAsicVisual
          variant="standard"
          title="ASIC Core"
          subtitle="Live hashboard activity, chain balance, temperature, and silicon health"
          actionLabel="Open hashboards"
          onAction={props.onOpenConfig}
        />
        <CurrentBlockCard compact />
      </section>

      {/* Kit: live autotuner — primary feedback when tuning runs. */}
      <AutotunerCard />

      {/* Operator Focus — production truth-driven "next action" panel. The
          kit has no equivalent slot; we keep it (it is load-bearing for the
          honest operator-guidance contract) on the kit's calm state-panel
          glass directly under the autotuner. */}
      <div className="standard-dashboard-pair">
        <ThermalPowerPostureCard variant="compact" />
        <MiningWorkPostureCard variant="compact" />
      </div>

      <div className={`state-panel compact operator-focus-panel ${props.nextAction.tone}`}>
        <div className="state-panel-row">
          <div className="state-panel-main">
            <div className="state-panel-badge" aria-hidden="true">
              {props.nextAction.tone === 'success' ? 'OK'
                : props.nextAction.tone === 'danger' || props.nextAction.tone === 'warning' ? '!'
                  : 'i'}
            </div>
            <div className="state-panel-copy">
              <div className="state-panel-title">Operator Focus</div>
              <div className="state-panel-message">
                <strong>{props.nextAction.label}</strong>
                <br />
                {props.nextAction.detail}
              </div>
            </div>
          </div>
          <button type="button" className="ops-action-btn" onClick={props.nextAction.onAction}>
            <span className="ops-action-label">{props.nextAction.actionLabel}</span>
          </button>
        </div>
      </div>

      {/* Proxy/native telemetry honesty card — power users only. */}
      {!betaView && honestState !== 'native' && (
        <div className="ds-shell-offset">
          <HonestModeCard compact />
        </div>
      )}

      {/* Kit: overview chart (real hashrate history). */}
      <KitOverviewChart />

      {/* Kit: [FanMonitor | StatsKpiGrid] — kit grid is 1.1fr / 2fr. Wave-13:
          moved off an inline grid style (invisible to the responsive collapse
          rule, so it stayed 2-col on mobile) onto the shared pair class + a
          --fan ratio modifier. */}
      <div className="standard-dashboard-pair standard-dashboard-pair--fan">
        <KitFanMonitor onConfigure={props.onOpenConfig} />
        <KitStatsKpiGrid />
      </div>

      {/* Kit: hash board strip — hashrate/temp/chips PLUS per-chain
          voltage/frequency detail, merged into one section (2026-06-25, per
          operator: the two cards were repetitive). */}
      <KitHashBoardStrip />

      <PlatformOverviewCard />

      {/* RALPH competitive/contract gates — power users only (not beta). */}
      {!betaView && (
        <div className="standard-dashboard-pair">
          <CompetitiveReadinessCard compact />
          <MiningPipelineManifestCard compact />
        </div>
      )}

      <section className="section">
        <h3 className="section-title">Network &amp; Identity</h3>
        <NetworkContextCard />
      </section>
    </div>
  );
}
