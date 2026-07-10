// Green Mining Page — "Mining as a Battery" + Solar + Methane + Carbon metrics
// Feature no competitor has: framing mining as energy storage + environmental impact tracking.
//
// TRUTH-CONTRACT NOTE (): this page renders ONLY live-derived figures as
// fact (energy spent in kWh, uptime hours, wall efficiency in W/TH — all from
// live watts/uptime/hashrate). Numbers that would require a backend feed — the
// USD value of stored work, a real conversion-efficiency model, solar/green
// hours, and the operator's ACTUAL grid carbon intensity — are shown as "not
// yet available" or as a clearly-labeled reference, never as a fabricated
// number. The GreenMiningMetrics API type exists (api/feature-types.ts) but the
// daemon does not populate it yet; when it does, wire the real values here.

import React from 'react';
import { useTranslation } from '../../i18n/i18n';
import { useMinerStore } from '../../store/miner';
import { InfoDot } from '../common/Tooltip';
import { MethaneCalculator } from './MethaneCalculator';
import { SolarConfig } from './SolarConfig';
import { getLiveWallWatts } from '../../utils/power';

// A neutral unavailable marker for figures that need a backend feed.
const NA = '—';
// Reference grid carbon for the gauge illustration — NOT the operator's measured
// grid. Labeled as a reference on-screen until a region/grid feed is configured.
const REF_GRID_CARBON_GCO2KWH = 40; // Quebec hydro-dominant reference

export function GreenMiningPage() {
  const { t } = useTranslation();
  const stats = useMinerStore(s => s.stats);

  const watts = getLiveWallWatts(stats?.power);
  const hasLiveWallPower = watts > 0;
  const hashrateThs = stats?.hashrate_ths ?? 0;
  const uptimeS = stats?.uptime_s ?? 0;

  // Live-derived, honest figures only.
  const energyStoredTodayKwh = hasLiveWallPower
    ? (watts * uptimeS) / 3600 / 1000
    : null; // wall kWh spent today
  const uptimeHoursToday = Math.min(24, uptimeS / 3600);
  // Wall efficiency (W per TH/s) — a real, standard miner metric. Null until the
  // miner reports both wall power and hashrate (no fabricated percentage).
  const wallEfficiencyWPerTh =
    hashrateThs > 0 && hasLiveWallPower ? watts / hashrateThs : null;

  return (
    <div className="feat-page">
      <div className="feat-header">
        <h2 className="feat-title feat-title-green">{t('green.title')}</h2>
      </div>

      {/* Mining as a Battery — Hero Section */}
      <div className="feat-card feat-battery-hero">
        <h3 className="feat-battery-headline">{t('green.batteryHeadline')}</h3>
        <p className="feat-battery-subtitle">{t('green.batterySubtitle')}</p>

        {/* Battery visual — the honest "stored" quantity is ENERGY (kWh you have
            spent today), derived from live wall watts × uptime. The USD value of
            that stored work needs an earnings feed and is not asserted here. */}
        <div className="feat-battery-visual">
          <div className="feat-battery-shell">
            <div className="feat-battery-cap" />
            <div className="feat-battery-body">
              <div
                className="feat-battery-fill"
                style={{
                  height: `${energyStoredTodayKwh != null ? Math.min(100, (energyStoredTodayKwh / 26.4) * 100) : 0}%`,
                }}
              />
              <div className="feat-battery-label">
                <div className="feat-battery-value">
                  {energyStoredTodayKwh != null ? energyStoredTodayKwh.toFixed(2) : NA}
                </div>
                <div className="feat-battery-unit">kWh stored today</div>
              </div>
            </div>
          </div>
          <div className="feat-battery-metrics">
            <div className="feat-metric-card feat-metric-green">
              <div className="feat-metric-label">Uptime today</div>
              <div className="feat-metric-value">{uptimeHoursToday.toFixed(1)} h</div>
            </div>
            <div className="feat-metric-card feat-metric-green">
              <div className="feat-metric-label">
                Wall efficiency
                <InfoDot
                  placement="top"
                  label="What wall efficiency means"
                  content={
                    <>
                      Watts drawn at the wall per TH/s of hashrate (lower is more
                      efficient). Derived live from the miner&apos;s reported wall
                      power and hashrate — it does not change the heat you get.
                    </>
                  }
                />
              </div>
              <div className="feat-metric-value">
                {wallEfficiencyWPerTh != null
                  ? `${wallEfficiencyWPerTh.toFixed(1)} W/TH`
                  : NA}
              </div>
            </div>
          </div>
        </div>
        {!hasLiveWallPower && (
          <div style={{ marginTop: 10, fontSize: '0.72rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
            Energy and efficiency remain unavailable until live wall-power telemetry is reported.
          </div>
        )}

        {/* vs Battery Storage comparison */}
        <div className="feat-comparison">
          <h4 className="feat-comparison-title">{t('green.vsBattery')}</h4>
          <div className="feat-comparison-grid">
            <div className="feat-comparison-item">
              <div className="feat-comparison-icon feat-icon-green">&#x2713;</div>
              <span>{t('green.noDegradation')}</span>
            </div>
            <div className="feat-comparison-item">
              <div className="feat-comparison-icon feat-icon-green">&#x2713;</div>
              <span>{t('green.unlimitedCapacity')}</span>
            </div>
            <div className="feat-comparison-item">
              <div className="feat-comparison-icon feat-icon-green">&#x2713;</div>
              <span>{t('green.earnsRevenue')}</span>
            </div>
          </div>
        </div>
      </div>

      {/* Carbon Intensity Section — REFERENCE illustration (Quebec hydro grid),
          NOT the operator's measured grid. Labeled as such so no green grade is
          ever asserted as the user's own until a region/grid feed exists. */}
      <div className="feat-card">
        <h3 className="feat-card-title">{t('green.carbonIntensity')}</h3>
        <div className="feat-carbon-row">
          <div className="feat-carbon-gauge">
            <svg viewBox="0 0 120 60" width="120" height="60">
              {/* Background arc */}
              <path
                d="M 10 55 A 50 50 0 0 1 110 55"
                fill="none"
                stroke="var(--border)"
                strokeWidth="8"
                strokeLinecap="round"
              />
              {/* Filled arc — green for low carbon */}
              <path
                d="M 10 55 A 50 50 0 0 1 110 55"
                fill="none"
                stroke="var(--feat-green)"
                strokeWidth="8"
                strokeLinecap="round"
                strokeDasharray={`${Math.min(1, REF_GRID_CARBON_GCO2KWH / 800) * 157} 157`}
              />
            </svg>
            <div className="feat-carbon-value">
              {REF_GRID_CARBON_GCO2KWH}
              <span className="feat-carbon-unit">{t('green.gco2kwh')}</span>
            </div>
          </div>
          <div className="feat-carbon-info">
            <div className="feat-carbon-score">
              <div className="feat-metric-label">
                Reference grid
                <InfoDot
                  placement="top"
                  label="About this reference grid"
                  content={
                    <>
                      This is a reference grid (hydro-heavy Quebec, ~40 gCO₂/kWh),
                      not your measured grid. A green score for your own location
                      will appear once a grid/region feed is connected. The grade
                      reflects the grid, not the miner.
                    </>
                  }
                />
              </div>
              <div className="feat-metric-value feat-value-green">A+ (reference)</div>
            </div>
            <div className="feat-carbon-hours">
              <div className="feat-metric-label">{t('green.greenHours')}</div>
              <div className="feat-metric-value">
                {NA} / {uptimeHoursToday.toFixed(1)} h
              </div>
            </div>
          </div>
        </div>
      </div>

      {/* Solar Config Section */}
      <SolarConfig />

      {/* Methane Calculator Section */}
      <MethaneCalculator />
    </div>
  );
}
