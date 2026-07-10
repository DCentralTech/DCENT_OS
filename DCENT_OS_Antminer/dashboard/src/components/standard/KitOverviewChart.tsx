// KitOverviewChart — structural recreation of the design-kit's
// `DashboardPage.jsx` OverviewChart section.
//
// Kit reference: ui_kits/dashboard/DashboardPage.jsx (OverviewChart):
//   <div className="section">
//     <div className="section-title">Overview <span className="small-tag">…</span></div>
//     <div className="chart-wrap"> <svg/> <div className="legend">…</div> </div>
//   </div>
//
// The kit's chart is a stylized fabricated multi-series SVG. We REPLACE the
// fabricated body with production's real `HashrateChart` (fed by the live
// hashrate history ring buffer in the store) — it already emits `.chart-wrap`.
// The kit's section/title/legend chrome is recreated 1:1 around it so the
// composition + class grammar matches the kit; the data stays 100% honest.
import React from 'react';
import { useMinerStore } from '../../store/miner';
import { useDashboardHealth } from '../../hooks/useDashboardHealth';
import { HashrateChart } from './HashrateChart';

export function KitOverviewChart() {
  const hashrateHistory = useMinerStore(s => s.hashrateHistory);
  const health = useDashboardHealth();
  const samples = hashrateHistory.length;
  const live = health.hasFreshTelemetry;

  return (
    <div className="section" data-testid="kit-overview-chart">
      <div className="section-title">
        Overview
        <span className="small-tag">
          {live ? 'live · realtime polling' : samples > 0 ? 'last samples' : 'awaiting telemetry'}
        </span>
      </div>
      <div className="chart-wrap">
        {/* Real hashrate history — HashrateChart emits its own
            `.chart-wrap ds-hashrate-chart` body, its own `.chart-legend`
            (Hashrate + sample count), and an honest empty state when there is
            no telemetry. Wave-13: removed the duplicate `.legend` block that
            re-showed "Hashrate" + the sample count a second time. */}
        <HashrateChart />
      </div>
    </div>
  );
}
