import React, { useEffect, useState } from 'react';
import { useMinerStore } from '../../store/miner';
import { api } from '../../api/client';
import { NextStepsPanel } from '../common/NextStepsPanel';
import { useModeNavigation } from '../../hooks/useModeNavigation';
import { estimateHeatingOffset, estimateDailySats } from '../../utils/thermal';
import type { HistoryPoint } from '../../api/types';
import { getLiveHistoryPointWallWatts, getLiveWallWatts } from '../../utils/power';
import { glossaryText } from '../../utils/glossary';
import { SectionSkeleton } from '../common/skeletons/SectionSkeleton';
import { HeatingValueSummary } from './HeatingValueSummary';
import { HeaterRoomTempGraph } from './HeaterRoomTempGraph';

type Period = 'day' | 'week' | 'month';

interface DaySummary {
  label: string;
  sortKey: string;  // ISO date string for chronological sorting
  satsEarned: number;
  heatingValue: number;
  powerAvgW: number;
  hoursActive: number;
}

function buildDaySummaries(
  history: HistoryPoint[],
  intervalS: number,
  period: Period,
  electricityRate: number,
  btcPrice: number,
  networkDifficulty: number | null | undefined,
): DaySummary[] {
  if (history.length === 0) return [];

  // Group points by day
  const dayMap = new Map<string, HistoryPoint[]>();

  for (const point of history) {
    const d = new Date(point.timestamp * 1000);
    const key = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
    const arr = dayMap.get(key) ?? [];
    arr.push(point);
    dayMap.set(key, arr);
  }

  const summaries: DaySummary[] = [];
  const dayNames = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
  const monthNames = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

  for (const [key, points] of dayMap) {
    const d = new Date(key + 'T00:00:00');
    const livePowerWatts = points.map(getLiveHistoryPointWallWatts).filter(watts => watts > 0);
    const avgPower = livePowerWatts.length
      ? livePowerWatts.reduce((sum, watts) => sum + watts, 0) / livePowerWatts.length
      : 0;
    const livePowerHours = (livePowerWatts.length * intervalS) / 3600;
    const hoursActive = (points.length * intervalS) / 3600;
    const avgHashrateGhs = points.reduce((s, p) => s + p.hashrate_ghs, 0) / points.length;

    // HEATER-2: per-day sats estimate via the canonical difficulty-anchored
    // model (same helper HeaterEarningCard uses), scaled by the fraction of
    // the day the miner was active. Returns 0 when network difficulty is
    // unknown — no fabricated `satsPerThPerDay = 5` stub constant.
    const satsEarned = Math.round(
      estimateDailySats(avgHashrateGhs, networkDifficulty) * (hoursActive / 24),
    );

    const heatingValue = avgPower > 0
      ? estimateHeatingOffset(avgPower, livePowerHours, electricityRate)
      : 0;

    let label: string;
    if (period === 'day') {
      label = 'Today';
    } else if (period === 'week') {
      label = dayNames[d.getDay()];
    } else {
      label = `${monthNames[d.getMonth()]} ${d.getDate()}`;
    }

    summaries.push({ label, sortKey: key, satsEarned, heatingValue, powerAvgW: Math.round(avgPower), hoursActive });
  }

  // Sort chronologically by date key (ISO format sorts correctly)
  summaries.sort((a, b) => a.sortKey.localeCompare(b.sortKey));

  if (period === 'week') return summaries.slice(-7);
  if (period === 'month') return summaries.slice(-30);
  return summaries.slice(-1);
}

export function HistoryView() {
  const settings = useMinerStore(s => s.settings);
  const networkDifficulty = useMinerStore(s => s.heaterStatus?.network_difficulty);
  const { startTaskHandoff } = useModeNavigation();
  const [period, setPeriod] = useState<Period>('week');
  const [history, setHistory] = useState<HistoryPoint[]>([]);
  const [intervalS, setIntervalS] = useState(60);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    setLoading(true);
    api.getHeaterHistory()
      .then(res => {
        setHistory(res.history);
        setIntervalS(res.interval_s);
      })
      .catch(() => {
        // Show empty state but notify user
        useMinerStore.getState().addToast('Could not load history data', 'warning');
      })
      .finally(() => setLoading(false));
  }, [period]);

  const summaries = buildDaySummaries(history, intervalS, period, settings.electricityRate, settings.btcPrice, networkDifficulty);

  const totalSats = summaries.reduce((s, d) => s + d.satsEarned, 0);
  const totalHeating = summaries.reduce((s, d) => s + d.heatingValue, 0);
  const totalUsd = (totalSats / 100_000_000) * settings.btcPrice + totalHeating;

  // Find max bar value for scaling
  const maxValue = Math.max(1, ...summaries.map(d => (d.satsEarned / 100_000_000) * settings.btcPrice + d.heatingValue));

  // Kit `HeaterHistory` heat-delivered card uses a separate bar series
  // (kit fakes "kWh equivalent"). We use the REAL per-day heating value
  // already computed in `summaries` (estimateHeatingOffset from real
  // wall watts). No fabricated kWh series.
  const maxHeating = Math.max(0.01, ...summaries.map(d => d.heatingValue));

  return (
    <div className="history-view">
      <NextStepsPanel mode="heater" />

      {/*
        HeatingValueSummary — rolling-window heat-credit + electricity cost
        based on wall watts (W8.6, wave 8). Cypress pinned in heat_credit_wall_watts.cy.ts.
      */}
      <HeatingValueSummary />

      <h2 className="history-title">Heat &amp; Earnings History</h2>

      {/* Period selector */}
      <div className="history-period-selector" role="group" aria-label="History period">
        {(['day', 'week', 'month'] as Period[]).map(p => (
          <button
            key={p}
            className={`history-period-btn${period === p ? ' active' : ''}`}
            onClick={() => setPeriod(p)}
            aria-pressed={period === p}
          >
            {p.charAt(0).toUpperCase() + p.slice(1)}
          </button>
        ))}
      </div>

      {/* Recomposed into the kit `HeaterHistory` (.nest-history) grammar:
          per-day value bar card + heat-delivered card + real miner-temp
          sparkline + 3 totals. Every series is the REAL `summaries` /
          `history` already fetched — no fabricated kit demo data. The 3
          totals + profitability link stay visible in every state, exactly
          as before (contract preserved). */}
      <div className="nest-history history-nest">
        {loading ? (
          <div className="history-loading">
            <SectionSkeleton rows={4} data-testid="history-loading-skeleton" />
          </div>
        ) : summaries.length === 0 ? (
          <>
            <HeaterRoomTempGraph history={history} loading={loading} />
            <SessionActivity />
          </>
        ) : (
          <>
            {/* Card 1 — per-day combined value (kit "sats earned per day") */}
            <div className="nest-card history-nest-card">
              <div className="nest-card-eyebrow history-nest-eyebrow">
                <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M12 1v22M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6" />
                </svg>
                {summaries.length} day{summaries.length !== 1 ? 's' : ''} · total value per day
              </div>
              <div
                className="history-chart history-nest-chart"
                role="img"
                aria-label={`Heat and earnings bar chart — ${period} view, ${summaries.length} day${summaries.length !== 1 ? 's' : ''}`}
              >
                {summaries.map((day) => {
                  const dayValue = (day.satsEarned / 100_000_000) * settings.btcPrice + day.heatingValue;
                  const heightPct = maxValue > 0 ? (dayValue / maxValue) * 100 : 0;
                  return (
                    <div className="history-bar-wrapper" key={day.sortKey}>
                      <div className="history-bar-value">${dayValue.toFixed(2)}</div>
                      <div className="history-bar-track">
                        <div
                          className="history-bar-fill"
                          style={{ height: `${Math.max(4, heightPct)}%` }}
                        />
                      </div>
                      <div className="history-bar-label">{day.label}</div>
                    </div>
                  );
                })}
              </div>
            </div>

            {/* Card 2 — heat delivered (kit "Heat delivered"), REAL per-day
                heating-offset dollar value, not a fabricated kWh series */}
            <div className="nest-card history-nest-card history-heat-card">
              <div className="nest-card-eyebrow history-nest-eyebrow">
                <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M8.5 14.5A2.5 2.5 0 0 0 11 12c0-1.38-.5-2-1-3-1.072-2.143-.224-4.054 2-6 .5 2.5 2 4.9 4 6.5 2 1.6 3 3.5 3 5.5a7 7 0 1 1-14 0c0-1.153.433-2.294 1-3a2.5 2.5 0 0 0 2.5 2.5z" />
                </svg>
                Heat delivered (value of warmth produced)
              </div>
              <div className="nest-history-bars history-heat-bars">
                {summaries.map((day) => (
                  <div className="nest-history-bar history-heat-bar" key={day.sortKey}>
                    <span className="nest-history-bar-val history-heat-bar-val">
                      ${day.heatingValue.toFixed(2)}
                    </span>
                    <div
                      className="nest-history-bar-fill history-heat-bar-fill"
                      style={{ height: `${Math.max(4, (day.heatingValue / maxHeating) * 100)}%` }}
                    />
                    <span className="nest-history-bar-day history-heat-bar-day">{day.label}</span>
                  </div>
                ))}
              </div>
            </div>

            {/* Real miner temperature trend over the loaded window */}
            <HeaterRoomTempGraph history={history} loading={loading} />
          </>
        )}

        {/* 3 totals (kit `.nest-history-totals`) — same real totals, always
            visible (contract preserved), kit card grammar + production
            hooks both retained */}
        <div className="history-summary-cards nest-history-totals">
          <div
            className="history-summary-card nest-history-total"
            data-tooltip={glossaryText('sats_estimate')}
          >
            <div className="history-summary-label nest-history-total-label">Projected sats</div>
            <div className="history-summary-value nest-history-total-value">{totalSats.toLocaleString()}</div>
          </div>
          <div
            className="history-summary-card nest-history-total"
            data-tooltip="The dollar value of the heat this miner produced — heat you'd otherwise pay a space heater to make."
          >
            <div className="history-summary-label nest-history-total-label">Warmth produced</div>
            <div className="history-summary-value nest-history-total-value">${totalHeating.toFixed(2)}</div>
          </div>
          <div
            className="history-summary-card accent nest-history-total"
            data-tooltip="Bitcoin earned plus the value of the heat produced — the real combined return of running a space heater that mines."
          >
            <div className="history-summary-label nest-history-total-label">Total value</div>
            <div className="history-summary-value nest-history-total-value">${totalUsd.toFixed(2)}</div>
          </div>
        </div>

        <div className="history-profitability-link-wrap">
          <button
            onClick={() => { void startTaskHandoff('standard', 'earnings', { returnLabel: 'Back to Heat History' }); }}
            className="history-profitability-link"
          >
            Open Profitability Breakdown In Mining Mode
          </button>
        </div>
      </div>
    </div>
  );
}

/** Fallback when heater history API is empty — show session hashrate activity */
function SessionActivity() {
  const hashrateHistory = useMinerStore(s => s.hashrateHistory);
  const status = useMinerStore(s => s.status);
  const stats = useMinerStore(s => s.stats);

  const hashrate = status?.hashrate_ghs ?? 0;
  const watts = getLiveWallWatts(stats?.power);
  const uptimeS = status?.uptime_s ?? 0;
  const accepted = status?.accepted ?? 0;
  const isMining = hashrate > 0;

  // Simple sparkline from history
  const recent = hashrateHistory.slice(-30);
  const maxHr = Math.max(1, ...recent.map(p => p.value));

  if (!isMining && recent.length === 0) {
    return (
      <div className="history-empty">
        <div className="history-empty-icon" aria-hidden="true">
          <svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="var(--text-dim)" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <line x1="18" y1="20" x2="18" y2="10" />
            <line x1="12" y1="20" x2="12" y2="4" />
            <line x1="6" y1="20" x2="6" y2="14" />
          </svg>
        </div>
        <div>No activity yet</div>
        <div className="history-empty-hint">
          Turn on your heater to see activity
        </div>
      </div>
    );
  }

  return (
    <div className="session-activity-wrapper">
      <div className="session-activity-title">
        Session Activity
      </div>

      {/* Mini sparkline — decorative; real data shown in stat grid below */}
      {recent.length > 2 && (
        <div aria-hidden="true" className="session-sparkline">
          {recent.map((p, i) => {
            const h = Math.max(4, (p.value / maxHr) * 70);
            return (
              <div
                key={i}
                className="session-sparkline-bar"
                style={{
                  height: h,
                  opacity: 0.6 + (i / recent.length) * 0.4,
                }}
              />
            );
          })}
        </div>
      )}

      {/* Session stats — consumer-friendly labels */}
      <div className="session-stat-grid">
        <div
          className="session-stat-cell"
          data-tooltip="Trillions of hashes per second the miner is computing right now — its raw Bitcoin-search speed."
        >
          <div className="session-stat-value session-stat-value--accent">
            {(hashrate / 1000).toFixed(2)}
          </div>
          <div className="session-stat-label">TH/s now</div>
        </div>
        <div
          className="session-stat-cell"
          data-tooltip="Live wall power being drawn and turned into useful room heat right now."
        >
          <div className="session-stat-value">
            {/* HEATER-3: real reported wall draw only — no fabricated 156 W
                constant. Em-dash when the daemon hasn't reported power yet. */}
            {watts > 0 ? `${watts}W` : (isMining ? '—' : '0W')}
          </div>
          <div className="session-stat-label">Warmth output</div>
        </div>
        <div
          className="session-stat-cell"
          data-tooltip="Valid proofs the pool credited toward your payout. A rising count is the real proof you're earning."
        >
          <div className="session-stat-value session-stat-value--green">
            {accepted}
          </div>
          <div className="session-stat-label">Accepted shares</div>
        </div>
        <div
          className="session-stat-cell"
          data-tooltip="How long the miner has been running continuously since it last started."
        >
          <div className="session-stat-value">
            {uptimeS > 3600 ? `${Math.floor(uptimeS / 3600)}h` : `${Math.floor(uptimeS / 60)}m`}
          </div>
          <div className="session-stat-label">Hours mining</div>
        </div>
      </div>
    </div>
  );
}
